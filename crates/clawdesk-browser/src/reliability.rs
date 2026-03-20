//! CDP Reliability Layer — actionability gates, semantic retry, and network-idle hysteresis.
//!
//! Provides Playwright-equivalent browser reliability in pure Rust, layered on top
//! of the existing `CdpSession`. No external runtime dependencies.
//!
//! ## Four Reliability Behaviors
//!
//! **(a) Actionability Gate:** Before Click/Type/Fill/SelectOption, verify the target
//! element is visible, enabled, in-viewport, and positionally stable (~34ms per gate).
//!
//! **(b) Semantic Retry:** Classify CDP errors as retriable vs terminal. Retry with
//! exponential backoff for context-destroyed, stale-node errors. Terminal errors
//! (syntax, method-not-found) propagate immediately.
//!
//! **(c) Network Idle with Hysteresis:** Fire "idle" only after `pending_requests == 0`
//! for a continuous window of W milliseconds (default 500ms). Prevents false idle
//! during cascading XHR.
//!
//! **(d) Scroll-into-View:** When `inVp: false` in `DomSnapshot`, inject
//! `scrollIntoView({block:'center'})` before interaction.

use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

/// Configuration for the reliability layer.
#[derive(Debug, Clone)]
pub struct ReliabilityConfig {
    /// Maximum retries for retriable CDP errors.
    pub max_retries: u32,
    /// Base delay for exponential backoff (ms).
    pub retry_base_delay_ms: u64,
    /// Network idle hysteresis window (ms).
    pub idle_hysteresis_ms: u64,
    /// Positional stability threshold in pixels.
    pub stability_threshold_px: f64,
    /// Minimum opacity for visibility check.
    pub min_opacity: f64,
    /// Timeout for actionability check (ms).
    pub actionability_timeout_ms: u64,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_base_delay_ms: 200,
            idle_hysteresis_ms: 500,
            stability_threshold_px: 2.0,
            min_opacity: 0.1,
            actionability_timeout_ms: 5000,
        }
    }
}

/// CDP error classification for retry decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Error is transient and should be retried (context destroyed, stale node).
    Retriable,
    /// Error is permanent and should propagate immediately.
    Terminal,
}

/// Classify a CDP error code and message into retriable vs terminal.
///
/// Partition:
/// - Retriable: -32000 (generic server error / context destroyed),
///   -32602 (invalid params from stale nodeId)
/// - Terminal: -32601 (method not found), -32700 (parse error)
pub fn classify_cdp_error(code: i64, message: &str) -> ErrorClass {
    match code {
        -32000 => {
            let msg_lower = message.to_lowercase();
            if msg_lower.contains("context")
                || msg_lower.contains("detached")
                || msg_lower.contains("destroyed")
                || msg_lower.contains("not found")
                || msg_lower.contains("frame was detached")
                || msg_lower.contains("navigation")
            {
                ErrorClass::Retriable
            } else {
                ErrorClass::Terminal
            }
        }
        -32602 => ErrorClass::Retriable, // Invalid params (stale nodeId)
        -32601 => ErrorClass::Terminal,    // Method not found
        -32700 => ErrorClass::Terminal,    // Parse error
        // net:: errors from navigation (DNS failure, refused, etc.)
        -32000..=-31000 => ErrorClass::Retriable,
        _ => ErrorClass::Terminal,         // Unknown — don't retry
    }
}

/// Result of an actionability check on an element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionabilityResult {
    /// Whether the element passes all actionability checks.
    pub actionable: bool,
    /// Whether the element is visible (display, visibility, opacity).
    pub visible: bool,
    /// Whether the element is in the viewport.
    pub in_viewport: bool,
    /// Whether the element is enabled (not disabled, not aria-disabled).
    pub enabled: bool,
    /// Whether the element's position is stable (no CSS animation in progress).
    pub stable: bool,
    /// If not actionable, the reason why.
    pub reason: Option<String>,
}

/// JavaScript for the actionability check.
///
/// Injected as a single `Runtime.evaluate` call. Checks:
/// 1. Visibility: computed display ≠ none, visibility ≠ hidden, opacity ≥ threshold
/// 2. Viewport: element's bounding rect intersects the viewport
/// 3. Enabled: not disabled attribute, not aria-disabled="true"
/// 4. Returns the bounding rect for positional stability comparison
pub fn actionability_check_js(selector: &str, min_opacity: f64) -> String {
    format!(
        r#"(() => {{
  const el = document.querySelector('{selector}');
  if (!el) return {{ actionable: false, visible: false, in_viewport: false, enabled: false, stable: true, reason: 'element not found' }};
  
  const style = window.getComputedStyle(el);
  const rect = el.getBoundingClientRect();
  
  const visible = style.display !== 'none' 
    && style.visibility !== 'hidden' 
    && parseFloat(style.opacity) >= {min_opacity};
  
  const in_viewport = rect.bottom > 0 
    && rect.right > 0 
    && rect.top < window.innerHeight 
    && rect.left < window.innerWidth
    && rect.width > 0 
    && rect.height > 0;
  
  const enabled = !el.disabled 
    && el.getAttribute('aria-disabled') !== 'true';
  
  return {{
    actionable: visible && in_viewport && enabled,
    visible,
    in_viewport,
    enabled,
    stable: true,
    reason: !visible ? 'not visible' : !in_viewport ? 'not in viewport' : !enabled ? 'disabled' : null,
    rect: [rect.x, rect.y, rect.width, rect.height]
  }};
}})()"#,
        selector = selector.replace('\'', "\\'"),
        min_opacity = min_opacity
    )
}

/// JavaScript for positional stability check.
///
/// Uses `requestAnimationFrame` to compare element position across two frames.
/// If the delta is below the threshold, the element is considered stable.
/// Total wait: 2 × 16.67ms = ~34ms (two animation frames).
pub fn stability_check_js(selector: &str, threshold_px: f64) -> String {
    format!(
        r#"new Promise(resolve => {{
  const el = document.querySelector('{selector}');
  if (!el) return resolve({{ stable: false, reason: 'element not found' }});
  
  requestAnimationFrame(() => {{
    const r1 = el.getBoundingClientRect();
    requestAnimationFrame(() => {{
      const r2 = el.getBoundingClientRect();
      const dx = Math.abs(r2.x - r1.x);
      const dy = Math.abs(r2.y - r1.y);
      const delta = Math.sqrt(dx*dx + dy*dy);
      resolve({{
        stable: delta < {threshold},
        delta,
        rect: [r2.x, r2.y, r2.width, r2.height]
      }});
    }});
  }});
}})"#,
        selector = selector.replace('\'', "\\'"),
        threshold = threshold_px
    )
}

/// JavaScript for scrolling an element into view.
///
/// Uses `scrollIntoView({block:'center', behavior:'instant'})` for immediate
/// positioning without smooth-scroll animation delay.
pub fn scroll_into_view_js(selector: &str) -> String {
    format!(
        r#"(() => {{
  const el = document.querySelector('{selector}');
  if (el) {{
    el.scrollIntoView({{ block: 'center', behavior: 'instant' }});
    return true;
  }}
  return false;
}})()"#,
        selector = selector.replace('\'', "\\'")
    )
}

/// JavaScript for data-ci index-based scroll into view.
pub fn scroll_into_view_by_index_js(index: u32) -> String {
    format!(
        r#"(() => {{
  const el = document.querySelector('[data-ci="{index}"]');
  if (el) {{
    el.scrollIntoView({{ block: 'center', behavior: 'instant' }});
    return true;
  }}
  return false;
}})()"#,
        index = index
    )
}

/// JavaScript for the combined actionability + scrollIntoView + action batch.
///
/// Batches the actionability check, scroll-into-view, and the actual action
/// into one `Runtime.evaluate` call per element. Amortized per-action CDP
/// round-trips: 1 instead of 3.
pub fn batched_click_js(selector: &str, min_opacity: f64) -> String {
    format!(
        r#"(() => {{
  const el = document.querySelector('{selector}');
  if (!el) return {{ success: false, reason: 'element not found' }};
  
  const style = window.getComputedStyle(el);
  const rect = el.getBoundingClientRect();
  
  if (style.display === 'none' || style.visibility === 'hidden' || parseFloat(style.opacity) < {min_opacity})
    return {{ success: false, reason: 'not visible' }};
  
  if (el.disabled || el.getAttribute('aria-disabled') === 'true')
    return {{ success: false, reason: 'disabled' }};
  
  // Scroll into view if needed
  if (rect.bottom <= 0 || rect.top >= window.innerHeight || rect.right <= 0 || rect.left >= window.innerWidth) {{
    el.scrollIntoView({{ block: 'center', behavior: 'instant' }});
  }}
  
  el.click();
  return {{ success: true }};
}})()"#,
        selector = selector.replace('\'', "\\'"),
        min_opacity = min_opacity
    )
}

/// Network idle hysteresis tracker.
///
/// Implements a debounce filter on the idle signal: fires only after
/// `pending_requests == 0` for a continuous window of `hysteresis_ms`.
///
/// Formally: D(t) = min_{s ∈ [t-W, t]} I(s) where I(t) = (pending == 0).
#[derive(Debug, Clone)]
pub struct NetworkIdleTracker {
    /// Number of in-flight requests.
    pending_requests: u32,
    /// When the last request completed (for hysteresis timing).
    last_zero_time: Option<std::time::Instant>,
    /// Hysteresis window in milliseconds.
    hysteresis_ms: u64,
}

impl NetworkIdleTracker {
    pub fn new(hysteresis_ms: u64) -> Self {
        Self {
            pending_requests: 0,
            last_zero_time: None,
            hysteresis_ms,
        }
    }

    /// Called when a network request starts (CDP `Network.requestWillBeSent`).
    pub fn on_request_start(&mut self) {
        self.pending_requests += 1;
        self.last_zero_time = None; // Reset hysteresis
    }

    /// Called when a network request completes (CDP `Network.loadingFinished`/`Failed`).
    pub fn on_request_end(&mut self) {
        self.pending_requests = self.pending_requests.saturating_sub(1);
        if self.pending_requests == 0 {
            self.last_zero_time = Some(std::time::Instant::now());
        }
    }

    /// Check if the network has been idle for the full hysteresis window.
    pub fn is_idle_with_hysteresis(&self) -> bool {
        if self.pending_requests > 0 {
            return false;
        }
        match self.last_zero_time {
            Some(t) => t.elapsed().as_millis() >= self.hysteresis_ms as u128,
            None => false,
        }
    }

    /// Current number of pending requests.
    pub fn pending(&self) -> u32 {
        self.pending_requests
    }
}

/// Execute a CDP command with semantic retry and exponential backoff.
///
/// Classifies errors as retriable or terminal. Retriable errors are retried
/// up to `max_retries` times with exponential backoff (base_delay × 2^attempt).
///
/// For base_delay=200ms and max_retries=3, worst-case retry time: 1.4s.
pub async fn with_retry<F, Fut, T>(
    config: &ReliabilityConfig,
    operation_name: &str,
    mut f: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, (i64, String)>>,
{
    let mut attempt = 0;
    loop {
        match f().await {
            Ok(value) => return Ok(value),
            Err((code, message)) => {
                let class = classify_cdp_error(code, &message);
                if class == ErrorClass::Terminal || attempt >= config.max_retries {
                    return Err(format!(
                        "{} failed (code {}): {}",
                        operation_name, code, message
                    ));
                }
                let delay = Duration::from_millis(
                    config.retry_base_delay_ms * 2u64.pow(attempt),
                );
                warn!(
                    operation = operation_name,
                    attempt = attempt + 1,
                    max = config.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    code,
                    "retriable CDP error, backing off"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_context_destroyed() {
        assert_eq!(
            classify_cdp_error(-32000, "Cannot find context with specified id"),
            ErrorClass::Retriable
        );
    }

    #[test]
    fn classify_detached_node() {
        assert_eq!(
            classify_cdp_error(-32000, "Node is detached from document"),
            ErrorClass::Retriable
        );
    }

    #[test]
    fn classify_stale_params() {
        assert_eq!(
            classify_cdp_error(-32602, "Could not find node with given id"),
            ErrorClass::Retriable
        );
    }

    #[test]
    fn classify_method_not_found() {
        assert_eq!(
            classify_cdp_error(-32601, "'Foo.bar' wasn't found"),
            ErrorClass::Terminal
        );
    }

    #[test]
    fn classify_parse_error() {
        assert_eq!(
            classify_cdp_error(-32700, "Parse error"),
            ErrorClass::Terminal
        );
    }

    #[test]
    fn network_idle_hysteresis() {
        let mut tracker = NetworkIdleTracker::new(500);
        assert!(!tracker.is_idle_with_hysteresis()); // No zero time
        
        tracker.on_request_start();
        assert!(!tracker.is_idle_with_hysteresis());
        
        tracker.on_request_end();
        // Just reached zero — hysteresis not elapsed
        assert!(!tracker.is_idle_with_hysteresis());
    }

    #[test]
    fn network_idle_reset_on_new_request() {
        let mut tracker = NetworkIdleTracker::new(0); // Instant hysteresis for test
        tracker.on_request_start();
        tracker.on_request_end();
        // Would be idle, but start a new request
        tracker.on_request_start();
        assert!(!tracker.is_idle_with_hysteresis());
    }

    #[test]
    fn actionability_check_js_output() {
        let js = actionability_check_js("[data-ci='5']", 0.1);
        assert!(js.contains("getComputedStyle"));
        assert!(js.contains("getBoundingClientRect"));
        assert!(js.contains("0.1")); // min opacity
    }

    #[test]
    fn stability_check_js_output() {
        let js = stability_check_js("[data-ci='5']", 2.0);
        assert!(js.contains("requestAnimationFrame"));
        assert!(js.contains("2")); // threshold
    }

    #[test]
    fn scroll_into_view_js_output() {
        let js = scroll_into_view_js(".my-element");
        assert!(js.contains("scrollIntoView"));
        assert!(js.contains("center"));
        assert!(js.contains("instant"));
    }

    #[test]
    fn batched_click_includes_all_steps() {
        let js = batched_click_js("[data-ci='10']", 0.1);
        // Should include visibility check, scroll, and click
        assert!(js.contains("getComputedStyle"));
        assert!(js.contains("scrollIntoView"));
        assert!(js.contains(".click()"));
    }

    #[test]
    fn scroll_by_index() {
        let js = scroll_into_view_by_index_js(42);
        assert!(js.contains("data-ci=\"42\""));
        assert!(js.contains("scrollIntoView"));
    }
}
