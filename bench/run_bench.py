#!/usr/bin/env python3
"""
ClawDesk Memory Benchmark Runner

MemoryAgentBench-style evaluation of ClawDesk's memory system.
Tests 5 competencies: Accurate Retrieval, Test-Time Learning,
Long-Range Understanding, Conflict Resolution, Semantic Similarity.

Usage:
    # Run all benchmarks
    python run_bench.py

    # Run specific competency
    python run_bench.py --competency accurate_retrieval

    # Custom Ollama URL
    python run_bench.py --ollama-url http://192.168.1.198:11434

    # Output JSON results
    python run_bench.py --json results.json
"""

import argparse
import json
import os
import sys
import time
from datetime import datetime
from pathlib import Path

from bridge import BridgeConfig, ClawDeskBridge
from datasets import ALL_BENCHMARKS, BenchmarkCase
from evaluate import (
    BenchmarkResult,
    BenchmarkSummary,
    exact_match,
    mrr,
    recall_at_k,
    rouge_l_f1,
    substring_match,
    summarize_results,
    token_f1,
)


def run_case(
    bridge: ClawDeskBridge,
    case: BenchmarkCase,
    verbose: bool = False,
) -> list[BenchmarkResult]:
    """Run a single benchmark case: memorize chunks, then test queries."""
    results = []

    # ── Phase 1: Memorization ──
    if verbose:
        print(f"\n  [{case.id}] Memorizing {len(case.context_chunks)} chunks...", file=sys.stderr)

    t0 = time.time()
    items = [(chunk, "document", {}) for chunk in case.context_chunks]
    try:
        ids = bridge.remember_batch(items)
        memorize_time = time.time() - t0
        if verbose:
            print(f"    Stored {len(ids)} memories in {memorize_time:.1f}s", file=sys.stderr)
    except Exception as e:
        print(f"    ERROR memorizing: {e}", file=sys.stderr)
        # Still try queries even if batch failed — try individual inserts
        for chunk in case.context_chunks:
            try:
                bridge.remember(chunk)
            except Exception:
                pass

    # Small delay for embedding indexing
    time.sleep(0.3)

    # ── Phase 2: Query & Evaluate ──
    for qi, (query, ground_truth) in enumerate(case.queries):
        t0 = time.time()
        try:
            recall_results = bridge.recall(query, max_results=10)
        except Exception as e:
            print(f"    ERROR recalling '{query}': {e}", file=sys.stderr)
            recall_results = []
        latency = (time.time() - t0) * 1000  # ms

        # Best matching content from recall
        best_content = ""
        best_score = 0.0
        if recall_results:
            best = recall_results[0]
            best_content = best.content or ""
            best_score = best.score

        # Compute metrics
        result = BenchmarkResult(
            test_id=f"{case.id}_q{qi:02d}",
            competency=case.competency,
            query=query,
            ground_truth=ground_truth,
            recalled_content=best_content[:200] if best_content else None,
            recall_score=best_score,
            exact_match=exact_match(best_content, ground_truth) if best_content else 0.0,
            substring_match=substring_match(best_content, ground_truth) if best_content else 0.0,
            f1=token_f1(best_content, ground_truth) if best_content else 0.0,
            recall_at_1=recall_at_k(recall_results, ground_truth, k=1),
            recall_at_5=recall_at_k(recall_results, ground_truth, k=5),
            mrr=mrr(recall_results, ground_truth),
            rouge_l=rouge_l_f1(best_content, ground_truth) if best_content else 0.0,
            latency_ms=latency,
            num_results=len(recall_results),
        )
        results.append(result)

        if verbose:
            status = "✓" if result.substring_match > 0 else "✗"
            print(
                f"    {status} Q: {query[:50]:50s} | "
                f"sub={result.substring_match:.0f} f1={result.f1:.2f} "
                f"score={result.recall_score:.3f} "
                f"({result.latency_ms:.0f}ms)",
                file=sys.stderr,
            )

    return results


def run_competency(
    bridge: ClawDeskBridge,
    competency: str,
    cases: list[BenchmarkCase],
    verbose: bool = False,
) -> BenchmarkSummary:
    """Run all cases for a competency, using unique DB per case for isolation."""
    all_results = []

    print(f"\n{'━'*60}", file=sys.stderr)
    print(f"  COMPETENCY: {competency.upper().replace('_', ' ')}", file=sys.stderr)
    print(f"  Cases: {len(cases)}", file=sys.stderr)
    print(f"{'━'*60}", file=sys.stderr)

    for ci, case in enumerate(cases):
        # Use a unique DB path per case for isolation (avoids lock conflicts)
        case_db = f"{bridge.config.db_path}_{competency}_{ci}"
        import shutil
        if os.path.exists(case_db):
            shutil.rmtree(case_db, ignore_errors=True)

        bridge.init(db_path=case_db)
        time.sleep(0.2)

        case_results = run_case(bridge, case, verbose=verbose)
        all_results.extend(case_results)

    summary = summarize_results(all_results, competency)
    print(str(summary), file=sys.stderr)
    return summary


def main():
    parser = argparse.ArgumentParser(
        description="ClawDesk Memory Benchmark — MemoryAgentBench-style evaluation"
    )
    parser.add_argument(
        "--competency", "-c",
        choices=list(ALL_BENCHMARKS.keys()) + ["all"],
        default="all",
        help="Which competency to benchmark (default: all)",
    )
    parser.add_argument(
        "--ollama-url",
        default="http://localhost:11434",
        help="Ollama base URL (default: http://localhost:11434)",
    )
    parser.add_argument(
        "--model",
        default="nomic-embed-text",
        help="Embedding model name (default: nomic-embed-text)",
    )
    parser.add_argument(
        "--db-path",
        default="/tmp/clawdesk_bench_sochdb",
        help="SochDB database path (default: /tmp/clawdesk_bench_sochdb)",
    )
    parser.add_argument(
        "--min-relevance",
        type=float,
        default=0.05,
        help="Minimum relevance score for recall (default: 0.05)",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Show per-query results",
    )
    parser.add_argument(
        "--json",
        type=str,
        default=None,
        help="Write JSON results to file",
    )
    args = parser.parse_args()

    # Build config
    config = BridgeConfig(
        db_path=args.db_path,
        ollama_url=args.ollama_url,
        model=args.model,
        min_relevance=args.min_relevance,
    )

    # Select competencies
    if args.competency == "all":
        competencies = list(ALL_BENCHMARKS.keys())
    else:
        competencies = [args.competency]

    print("╔════════════════════════════════════════════════════════════╗", file=sys.stderr)
    print("║     ClawDesk Memory Benchmark (MemoryAgentBench-style)   ║", file=sys.stderr)
    print("╠════════════════════════════════════════════════════════════╣", file=sys.stderr)
    print(f"║  Ollama URL   : {args.ollama_url:41s} ║", file=sys.stderr)
    print(f"║  Model        : {args.model:41s} ║", file=sys.stderr)
    print(f"║  DB Path      : {args.db_path:41s} ║", file=sys.stderr)
    print(f"║  Competencies : {len(competencies):41d} ║", file=sys.stderr)
    print("╚════════════════════════════════════════════════════════════╝", file=sys.stderr)

    all_summaries = []
    start_time = time.time()

    with ClawDeskBridge(config) as bridge:
        bridge.init()

        for comp_name in competencies:
            cases = ALL_BENCHMARKS[comp_name]
            summary = run_competency(bridge, comp_name, cases, verbose=args.verbose)
            all_summaries.append(summary)

    elapsed = time.time() - start_time

    # ── Overall Summary ──
    print("\n" + "═" * 60, file=sys.stderr)
    print("  OVERALL SUMMARY", file=sys.stderr)
    print("═" * 60, file=sys.stderr)

    total_tests = sum(s.num_tests for s in all_summaries)
    if total_tests > 0:
        weights = [s.num_tests for s in all_summaries]
        total_w = sum(weights)

        def wavg(attr):
            return sum(getattr(s, attr) * w for s, w in zip(all_summaries, weights)) / total_w

        print(f"  Total Tests     : {total_tests}", file=sys.stderr)
        print(f"  Total Time      : {elapsed:.1f}s", file=sys.stderr)
        print(f"  Avg Exact Match : {wavg('avg_exact_match'):.3f}", file=sys.stderr)
        print(f"  Avg Substr Match: {wavg('avg_substring_match'):.3f}", file=sys.stderr)
        print(f"  Avg Token F1    : {wavg('avg_f1'):.3f}", file=sys.stderr)
        print(f"  Avg Recall@1    : {wavg('avg_recall_at_1'):.3f}", file=sys.stderr)
        print(f"  Avg Recall@5    : {wavg('avg_recall_at_5'):.3f}", file=sys.stderr)
        print(f"  Avg MRR         : {wavg('avg_mrr'):.3f}", file=sys.stderr)
        print(f"  Avg ROUGE-L     : {wavg('avg_rouge_l'):.3f}", file=sys.stderr)
        print(f"  Avg Latency     : {wavg('avg_latency_ms'):.1f} ms", file=sys.stderr)
        print(f"  Bridge Avg Lat  : {bridge.avg_latency_ms:.1f} ms", file=sys.stderr)
        print(f"  Bridge P99 Lat  : {bridge.p99_latency_ms:.1f} ms", file=sys.stderr)
    print("═" * 60, file=sys.stderr)

    # ── JSON output ──
    if args.json:
        output = {
            "timestamp": datetime.now().isoformat(),
            "config": {
                "ollama_url": args.ollama_url,
                "model": args.model,
                "db_path": args.db_path,
                "min_relevance": args.min_relevance,
            },
            "elapsed_seconds": elapsed,
            "total_tests": total_tests,
            "competencies": {
                s.competency: {
                    "num_tests": s.num_tests,
                    "exact_match": round(s.avg_exact_match, 4),
                    "substring_match": round(s.avg_substring_match, 4),
                    "f1": round(s.avg_f1, 4),
                    "recall_at_1": round(s.avg_recall_at_1, 4),
                    "recall_at_5": round(s.avg_recall_at_5, 4),
                    "mrr": round(s.avg_mrr, 4),
                    "rouge_l": round(s.avg_rouge_l, 4),
                    "latency_ms": round(s.avg_latency_ms, 1),
                }
                for s in all_summaries
            },
        }
        with open(args.json, "w") as f:
            json.dump(output, f, indent=2)
        print(f"\n  Results written to {args.json}", file=sys.stderr)


if __name__ == "__main__":
    main()
