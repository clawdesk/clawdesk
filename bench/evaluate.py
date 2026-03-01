"""
Evaluation Metrics for Memory Benchmarks

Implements MemoryAgentBench-style metrics:
- exact_match: Exact string match
- substring_match: Answer appears in recalled content
- f1: Token-level F1 score
- recall_at_k: Whether the ground truth is in top-k results
- mrr: Mean Reciprocal Rank
- rouge_l: ROUGE-L F1 score
"""

import re
import string
from collections import Counter
from dataclasses import dataclass
from typing import Optional


def normalize_text(text: str) -> str:
    """Normalize text for comparison: lowercase, strip punctuation and extra whitespace."""
    text = text.lower()
    text = text.translate(str.maketrans("", "", string.punctuation))
    text = re.sub(r"\s+", " ", text).strip()
    return text


def exact_match(prediction: str, ground_truth: str) -> float:
    """Exact string match after normalization. Returns 1.0 or 0.0."""
    return 1.0 if normalize_text(prediction) == normalize_text(ground_truth) else 0.0


def substring_match(prediction: str, ground_truth: str) -> float:
    """Check if ground truth appears as substring in prediction. Returns 1.0 or 0.0."""
    return 1.0 if normalize_text(ground_truth) in normalize_text(prediction) else 0.0


def token_f1(prediction: str, ground_truth: str) -> float:
    """Token-level F1 score."""
    pred_tokens = normalize_text(prediction).split()
    truth_tokens = normalize_text(ground_truth).split()

    if not pred_tokens or not truth_tokens:
        return 1.0 if pred_tokens == truth_tokens else 0.0

    common = Counter(pred_tokens) & Counter(truth_tokens)
    num_common = sum(common.values())

    if num_common == 0:
        return 0.0

    precision = num_common / len(pred_tokens)
    recall = num_common / len(truth_tokens)
    f1 = 2 * precision * recall / (precision + recall)
    return f1


def recall_at_k(
    results: list,
    ground_truth: str,
    k: int = 5,
) -> float:
    """Whether the ground truth appears in top-k recalled results.

    Returns 1.0 if any of the top-k results contain the ground truth, else 0.0.
    """
    gt_norm = normalize_text(ground_truth)
    for r in results[:k]:
        content = r.content if hasattr(r, "content") else str(r)
        if content and gt_norm in normalize_text(content):
            return 1.0
    return 0.0


def mrr(
    results: list,
    ground_truth: str,
) -> float:
    """Mean Reciprocal Rank: 1/(rank of first relevant result).

    Returns 0.0 if not found.
    """
    gt_norm = normalize_text(ground_truth)
    for i, r in enumerate(results):
        content = r.content if hasattr(r, "content") else str(r)
        if content and gt_norm in normalize_text(content):
            return 1.0 / (i + 1)
    return 0.0


def rouge_l_f1(prediction: str, ground_truth: str) -> float:
    """ROUGE-L F1 score based on longest common subsequence."""
    pred_tokens = normalize_text(prediction).split()
    truth_tokens = normalize_text(ground_truth).split()

    if not pred_tokens or not truth_tokens:
        return 1.0 if pred_tokens == truth_tokens else 0.0

    # LCS length
    m, n = len(truth_tokens), len(pred_tokens)
    dp = [[0] * (n + 1) for _ in range(m + 1)]
    for i in range(1, m + 1):
        for j in range(1, n + 1):
            if truth_tokens[i - 1] == pred_tokens[j - 1]:
                dp[i][j] = dp[i - 1][j - 1] + 1
            else:
                dp[i][j] = max(dp[i - 1][j], dp[i][j - 1])
    lcs_len = dp[m][n]

    if lcs_len == 0:
        return 0.0

    precision = lcs_len / n
    recall = lcs_len / m
    f1 = 2 * precision * recall / (precision + recall)
    return f1


@dataclass
class BenchmarkResult:
    """Result of a single benchmark test case."""
    test_id: str
    competency: str
    query: str
    ground_truth: str
    recalled_content: Optional[str]
    recall_score: float
    exact_match: float
    substring_match: float
    f1: float
    recall_at_1: float
    recall_at_5: float
    mrr: float
    rouge_l: float
    latency_ms: float
    num_results: int


@dataclass
class BenchmarkSummary:
    """Aggregated benchmark results."""
    competency: str
    num_tests: int
    avg_exact_match: float
    avg_substring_match: float
    avg_f1: float
    avg_recall_at_1: float
    avg_recall_at_5: float
    avg_mrr: float
    avg_rouge_l: float
    avg_latency_ms: float
    avg_recall_score: float

    def __str__(self) -> str:
        return (
            f"{'='*60}\n"
            f" {self.competency} ({self.num_tests} tests)\n"
            f"{'='*60}\n"
            f"  Exact Match     : {self.avg_exact_match:.3f}\n"
            f"  Substring Match : {self.avg_substring_match:.3f}\n"
            f"  Token F1        : {self.avg_f1:.3f}\n"
            f"  Recall@1        : {self.avg_recall_at_1:.3f}\n"
            f"  Recall@5        : {self.avg_recall_at_5:.3f}\n"
            f"  MRR             : {self.avg_mrr:.3f}\n"
            f"  ROUGE-L         : {self.avg_rouge_l:.3f}\n"
            f"  Avg Score       : {self.avg_recall_score:.3f}\n"
            f"  Avg Latency     : {self.avg_latency_ms:.1f} ms\n"
        )


def summarize_results(results: list[BenchmarkResult], competency: str) -> BenchmarkSummary:
    """Summarize a list of benchmark results."""
    n = len(results)
    if n == 0:
        return BenchmarkSummary(
            competency=competency, num_tests=0,
            avg_exact_match=0, avg_substring_match=0, avg_f1=0,
            avg_recall_at_1=0, avg_recall_at_5=0, avg_mrr=0,
            avg_rouge_l=0, avg_latency_ms=0, avg_recall_score=0,
        )
    return BenchmarkSummary(
        competency=competency,
        num_tests=n,
        avg_exact_match=sum(r.exact_match for r in results) / n,
        avg_substring_match=sum(r.substring_match for r in results) / n,
        avg_f1=sum(r.f1 for r in results) / n,
        avg_recall_at_1=sum(r.recall_at_1 for r in results) / n,
        avg_recall_at_5=sum(r.recall_at_5 for r in results) / n,
        avg_mrr=sum(r.mrr for r in results) / n,
        avg_rouge_l=sum(r.rouge_l for r in results) / n,
        avg_latency_ms=sum(r.latency_ms for r in results) / n,
        avg_recall_score=sum(r.recall_score for r in results) / n,
    )
