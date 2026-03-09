//! Const-generic typed vectors for compile-time dimension safety.
//!
//! Wraps the raw SIMD kernels in a `Vector<D>` type that enforces dimension
//! agreement at compile time — no runtime length checks needed.
//!
//! ```rust,ignore
//! let a: Vector<1536> = Vector::from_array([1.0; 1536]);
//! let b: Vector<1536> = Vector::from_array([0.5; 1536]);
//! let sim: f32 = a.cosine_similarity(&b);
//!
//! // let c: Vector<768> = Vector::from_array([0.0; 768]);
//! // a.cosine_similarity(&c); // Compile error! Dimension mismatch.
//! ```
//!
//! Common embedding dimensions are type-aliased:
//!
//! - `Vec384` (MiniLM)
//! - `Vec768` (BERT base)
//! - `Vec1024` (Cohere)
//! - `Vec1536` (OpenAI)
//! - `Vec3072` (OpenAI large)

use std::fmt;
use std::ops::{Add, Mul, Sub};

use crate::{cosine_similarity, dot_product, neg_euclidean_distance};

// ─────────────────────────────────────────────────────────────────────────────
// Core type
// ─────────────────────────────────────────────────────────────────────────────

/// A fixed-dimension embedding vector with compile-time dimension tracking.
///
/// `D` is the dimensionality (e.g., 1536 for OpenAI text-embedding-3-small).
/// All operations between two `Vector<D>` values are guaranteed at compile time
/// to operate on equally sized data — no runtime panics from length mismatch.
#[derive(Clone, PartialEq)]
pub struct Vector<const D: usize> {
    data: Vec<f32>,
}

impl<const D: usize> Vector<D> {
    /// Create from a fixed-size array (compile-time size guarantee).
    pub fn from_array(arr: [f32; D]) -> Self {
        Self {
            data: arr.to_vec(),
        }
    }

    /// Create from a slice, returning `None` if the length doesn't match `D`.
    pub fn from_slice(slice: &[f32]) -> Option<Self> {
        if slice.len() == D {
            Some(Self {
                data: slice.to_vec(),
            })
        } else {
            None
        }
    }

    /// Create from a `Vec<f32>`, returning `None` if the length doesn't match.
    pub fn from_vec(v: Vec<f32>) -> Option<Self> {
        if v.len() == D {
            Some(Self { data: v })
        } else {
            None
        }
    }

    /// Create a zero vector.
    pub fn zero() -> Self {
        Self {
            data: vec![0.0; D],
        }
    }

    /// Dimension (available at compile time via `D`, but also as a method).
    pub const fn dim() -> usize {
        D
    }

    /// Borrow the underlying data as a slice.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Consume and return the inner Vec.
    pub fn into_vec(self) -> Vec<f32> {
        self.data
    }

    // ── SIMD-accelerated operations ─────────────────────────────────────

    /// Cosine similarity with another vector of the same dimension.
    ///
    /// Returns a value in [-1.0, 1.0].
    pub fn cosine_similarity(&self, other: &Vector<D>) -> f32 {
        cosine_similarity(&self.data, &other.data)
    }

    /// Dot product with another vector of the same dimension.
    pub fn dot(&self, other: &Vector<D>) -> f32 {
        dot_product(&self.data, &other.data)
    }

    /// Negative Euclidean distance (higher = more similar).
    pub fn neg_euclidean(&self, other: &Vector<D>) -> f32 {
        neg_euclidean_distance(&self.data, &other.data)
    }

    /// L2 norm (Euclidean length).
    pub fn norm(&self) -> f32 {
        dot_product(&self.data, &self.data).sqrt()
    }

    /// Normalize to unit length. Returns zero vector if norm is zero.
    pub fn normalize(&self) -> Vector<D> {
        let n = self.norm();
        if n == 0.0 {
            return Vector::zero();
        }
        let inv = 1.0 / n;
        Vector {
            data: self.data.iter().map(|&x| x * inv).collect(),
        }
    }

    // ── Batch operations ────────────────────────────────────────────────

    /// Cosine similarity of this vector against a batch of vectors.
    ///
    /// More efficient than calling `cosine_similarity` in a loop because
    /// the query vector stays in L1/registers.
    pub fn batch_cosine(&self, others: &[Vector<D>]) -> Vec<f32> {
        let refs: Vec<&[f32]> = others.iter().map(|v| v.data.as_slice()).collect();
        crate::batch_cosine(&self.data, &refs)
    }

    /// Find the index of the most similar vector in a batch.
    pub fn nearest(&self, others: &[Vector<D>]) -> Option<(usize, f32)> {
        if others.is_empty() {
            return None;
        }
        let sims = self.batch_cosine(others);
        sims.iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, &s)| (i, s))
    }

    /// Find the top-k most similar vectors. Returns (index, similarity) pairs
    /// sorted by descending similarity.
    pub fn top_k(&self, others: &[Vector<D>], k: usize) -> Vec<(usize, f32)> {
        let sims = self.batch_cosine(others);
        let mut indexed: Vec<(usize, f32)> = sims.into_iter().enumerate().collect();
        indexed.sort_by(|(_, a), (_, b)| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        indexed.truncate(k);
        indexed
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Operator overloads
// ─────────────────────────────────────────────────────────────────────────────

impl<const D: usize> Add for &Vector<D> {
    type Output = Vector<D>;

    fn add(self, rhs: Self) -> Vector<D> {
        Vector {
            data: self
                .data
                .iter()
                .zip(rhs.data.iter())
                .map(|(&a, &b)| a + b)
                .collect(),
        }
    }
}

impl<const D: usize> Sub for &Vector<D> {
    type Output = Vector<D>;

    fn sub(self, rhs: Self) -> Vector<D> {
        Vector {
            data: self
                .data
                .iter()
                .zip(rhs.data.iter())
                .map(|(&a, &b)| a - b)
                .collect(),
        }
    }
}

/// Scalar multiplication.
impl<const D: usize> Mul<f32> for &Vector<D> {
    type Output = Vector<D>;

    fn mul(self, scalar: f32) -> Vector<D> {
        Vector {
            data: self.data.iter().map(|&x| x * scalar).collect(),
        }
    }
}

impl<const D: usize> fmt::Debug for Vector<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Vector<{D}>[")?;
        let show = D.min(4);
        for (i, val) in self.data[..show].iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{val:.4}")?;
        }
        if D > 4 {
            write!(f, ", ... ({} more)", D - 4)?;
        }
        write!(f, "]")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Common dimension aliases
// ─────────────────────────────────────────────────────────────────────────────

/// 384-dimensional vector (MiniLM, all-MiniLM-L6-v2).
pub type Vec384 = Vector<384>;
/// 768-dimensional vector (BERT base, nomic-embed-text).
pub type Vec768 = Vector<768>;
/// 1024-dimensional vector (Cohere embed-v3, BGE-large).
pub type Vec1024 = Vector<1024>;
/// 1536-dimensional vector (OpenAI text-embedding-3-small).
pub type Vec1536 = Vector<1536>;
/// 3072-dimensional vector (OpenAI text-embedding-3-large).
pub type Vec3072 = Vector<3072>;

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_time_dimension_match() {
        let a = Vector::<3>::from_array([1.0, 0.0, 0.0]);
        let b = Vector::<3>::from_array([0.0, 1.0, 0.0]);
        let sim = a.cosine_similarity(&b);
        assert!(sim.abs() < 1e-5);
    }

    #[test]
    fn from_slice_correct_len() {
        let data = vec![1.0, 2.0, 3.0];
        assert!(Vector::<3>::from_slice(&data).is_some());
        assert!(Vector::<4>::from_slice(&data).is_none());
    }

    #[test]
    fn normalize_unit_length() {
        let v = Vector::<3>::from_array([3.0, 4.0, 0.0]);
        let n = v.normalize();
        assert!((n.norm() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalize_zero_vector() {
        let v = Vector::<3>::zero();
        let n = v.normalize();
        assert_eq!(n.norm(), 0.0);
    }

    #[test]
    fn operator_add() {
        let a = Vector::<2>::from_array([1.0, 2.0]);
        let b = Vector::<2>::from_array([3.0, 4.0]);
        let c = &a + &b;
        assert_eq!(c.as_slice(), &[4.0, 6.0]);
    }

    #[test]
    fn operator_sub() {
        let a = Vector::<2>::from_array([5.0, 3.0]);
        let b = Vector::<2>::from_array([1.0, 1.0]);
        let c = &a - &b;
        assert_eq!(c.as_slice(), &[4.0, 2.0]);
    }

    #[test]
    fn scalar_mul() {
        let v = Vector::<3>::from_array([1.0, 2.0, 3.0]);
        let scaled = &v * 2.0;
        assert_eq!(scaled.as_slice(), &[2.0, 4.0, 6.0]);
    }

    #[test]
    fn nearest_basic() {
        let query = Vector::<3>::from_array([1.0, 0.0, 0.0]);
        let candidates = vec![
            Vector::<3>::from_array([0.0, 1.0, 0.0]),
            Vector::<3>::from_array([0.9, 0.1, 0.0]),
            Vector::<3>::from_array([-1.0, 0.0, 0.0]),
        ];
        let (idx, sim) = query.nearest(&candidates).unwrap();
        assert_eq!(idx, 1);
        assert!(sim > 0.9);
    }

    #[test]
    fn top_k_ordering() {
        let query = Vector::<2>::from_array([1.0, 0.0]);
        let candidates = vec![
            Vector::<2>::from_array([0.0, 1.0]),   // orthogonal
            Vector::<2>::from_array([1.0, 0.0]),   // identical
            Vector::<2>::from_array([0.7, 0.7]),   // similar
            Vector::<2>::from_array([-1.0, 0.0]),  // opposite
        ];
        let top = query.top_k(&candidates, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, 1); // identical first
        assert_eq!(top[1].0, 2); // similar second
    }

    #[test]
    fn type_alias_works() {
        let _: Vec1536 = Vector::zero();
        assert_eq!(Vec1536::dim(), 1536);
    }

    #[test]
    fn debug_format() {
        let v = Vector::<5>::from_array([1.0, 2.0, 3.0, 4.0, 5.0]);
        let s = format!("{v:?}");
        assert!(s.contains("Vector<5>"));
        assert!(s.contains("1 more"));
    }
}
