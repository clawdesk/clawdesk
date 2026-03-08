//! Dynamic Task Graph Generator (DTGG) with HEFT scheduling and replanning.
//!
//! Transforms static pipelines into adaptive computation graphs that evolve
//! at runtime. When a subtask fails or produces unexpected results, the
//! meta-planner rewrites the DAG — inserting new nodes, pruning failed
//! branches, and substituting alternative subgraphs.
//!
//! ## Algorithm
//!
//! The pipeline is modelled as a live DAG `G(t) = (V(t), E(t))` that evolves
//! with discrete time `t`. At each step completion the meta-planner evaluates
//! the residual subgoal set and applies graph rewriting rules:
//!
//! 1. **Node insertion** — `V(t+1) = V(t) ∪ {v_new}` when decomposition
//!    reveals a missing subtask.
//! 2. **Edge pruning** — `E(t+1) = E(t) \ {e_failed}` when a predecessor fails.
//! 3. **Subgraph substitution** — replace failed subgraph `S` with alternative
//!    `S′` preserving topological validity.
//!
//! Scheduling uses the **HEFT** (Heterogeneous Earliest Finish Time) algorithm:
//!
//! ```text
//! rank_u(i) = w̄_i + max_{j ∈ succ(i)} (c̄_{i,j} + rank_u(j))
//! ```
//!
//! giving `O(V² · P)` complexity for `P` processors.
//!
//! Replanning overhead is amortised `O(V + E)` per Kahn's algorithm
//! re-invocation — negligible relative to LLM inference latency.

pub mod dtgg;
pub mod heft;
pub mod rewrite;

pub use dtgg::*;
pub use heft::*;
pub use rewrite::*;
