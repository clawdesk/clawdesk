//! Integration tests for the Media crate.

#[cfg(test)]
mod tests {
    use clawdesk_media::cache_pro::{ClockProCache, CountingBloomFilter};
    use clawdesk_media::format::{FormatRouter, FormatCandidate};
    use clawdesk_media::error::{MediaError, MediaErrorKind};

    // ---- Phase 1: Format Routing ----

    #[test]
    fn phase1_format_routing_selects_processor() {
        let mut router = FormatRouter::new();
        router.register("image/png", FormatCandidate {
            name: "png-optimizer".into(),
            fidelity: 0.9,
            expected_latency_ms: 50,
            load_factor: 0.3,
            cache_warmth: 0.8,
        });
        router.register("image/jpeg", FormatCandidate {
            name: "jpeg-trans".into(),
            fidelity: 0.85,
            expected_latency_ms: 30,
            load_factor: 0.2,
            cache_warmth: 0.5,
        });

        let result = router.route("image/png");
        assert!(result.is_some());
        assert_eq!(result.unwrap().processor, "png-optimizer");
    }

    #[test]
    fn phase1_wildcard_fallback() {
        let mut router = FormatRouter::new();
        router.register("image/*", FormatCandidate {
            name: "generic-image".into(),
            fidelity: 0.7,
            expected_latency_ms: 100,
            load_factor: 0.1,
            cache_warmth: 0.0,
        });

        let result = router.route("image/webp");
        // image/* matches via the "image" segment
        assert!(result.is_some());
    }

    // ---- Phase 2: Clock-Pro Cache ----

    #[test]
    fn phase2_cache_stores_and_retrieves() {
        let mut cache = ClockProCache::new(100, 1000);
        cache.put("media-123".into(), vec![1u8, 2, 3], 3, 500, "audio/wav".into());

        let val = cache.get("media-123");
        assert!(val.is_some());
        assert_eq!(*val.unwrap(), vec![1u8, 2, 3]);
    }

    #[test]
    fn phase2_cache_evicts_under_pressure() {
        let mut cache = ClockProCache::new(1, 100); // 1 MB
        for i in 0..50 {
            cache.put(format!("entry-{i}"), format!("data-{i}"), 50_000, 10, "text/plain".into());
        }
        assert!(cache.len() < 50);
        assert!(cache.stats().evictions > 0);
    }

    // ---- Phase 3: Bloom Filter ----

    #[test]
    fn phase3_bloom_tracks_frequency() {
        let mut bloom = CountingBloomFilter::new(1000);
        for _ in 0..100 {
            bloom.increment("popular-item");
        }
        bloom.increment("rare-item");
        assert!(bloom.estimate("popular-item") > bloom.estimate("rare-item"));
        assert_eq!(bloom.estimate("never-seen"), 0);
    }

    // ---- Phase 4: Error Integration ----

    #[test]
    fn phase4_error_chain_through_pipeline() {
        let network_err = MediaError::new(MediaErrorKind::Network { detail: "connection refused".into() });
        let api_err = MediaError::new(MediaErrorKind::ProcessorApiFailed {
            processor: "whisper".into(),
            status: Some(500),
            detail: "internal".into(),
        }).with_source(Box::new(network_err));
        let pipeline_err = MediaError::new(MediaErrorKind::AllProcessorsFailed {
            media_type: "audio/wav".into(),
            attempts: 1,
        })
            .with_source(Box::new(api_err));

        let chain = pipeline_err.causal_chain();
        assert!(chain.len() >= 1);
    }

    #[test]
    fn phase4_format_error_detection() {
        let err = MediaError::new(MediaErrorKind::UnsupportedFormat { mime_type: "video/x-unknown".into() });
        assert!(err.is_format_error());
        assert!(!err.is_cache_error());
    }

    // ---- Phase 5: E2E ----

    #[test]
    fn phase5_route_then_cache_result() {
        let mut router = FormatRouter::new();
        router.register("audio/wav", FormatCandidate {
            name: "whisper".into(),
            fidelity: 0.95,
            expected_latency_ms: 200,
            load_factor: 0.4,
            cache_warmth: 0.0,
        });

        let route = router.route("audio/wav");
        assert!(route.is_some());

        let mut cache = ClockProCache::new(100, 1000);
        let result = "transcribed text from whisper";
        cache.put("audio-hash-abc123".into(), result.to_string(), result.len() as u64, 200, "audio/wav".into());

        let cached = cache.get("audio-hash-abc123");
        assert_eq!(cached, Some(&"transcribed text from whisper".to_string()));
        assert_eq!(cache.stats().hits, 1);
    }
}
