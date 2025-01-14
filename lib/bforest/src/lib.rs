//! A forest of B+-trees.
//!
//! This crate provides a data structures representing a set of small ordered sets or maps.
//! It is implemented as a forest of B+-trees all allocating nodes out of the same pool.
//!
//! **These are not general purpose data structures that are somehow magically faster that the
//! standard library's `BTreeSet` and `BTreeMap` types.**
//!
//! The tradeoffs are different:
//!
//! - Keys and values are expected to be small and copyable. We optimize for 32-bit types.
//! - A comparator object is used to compare keys, allowing smaller "context free" keys.
//! - Empty trees have a very small 32-bit footprint.
//! - All the trees in a forest can be cleared in constant time.

#![deny(missing_docs, trivial_numeric_casts)]
#![warn(unused_import_braces)]

use core::borrow::BorrowMut;
use core::cmp::Ordering;

use stdx::{impl_debug_display, impl_idx_from, packed_option};

mod map;
mod node;
mod path;
mod pool;
mod set;
#[cfg(test)]
mod tests;

pub use self::map::{Map, MapCursor, MapForest, MapIter};
use self::node::NodeData;
use self::path::Path;
use self::pool::NodePool;
pub use self::set::{RevSetIter, Set, SetCursor, SetForest, SetIter};

/// The maximum branching factor of an inner node in a B+-tree.
/// The minimum number of outgoing edges is `INNER_SIZE/2`.
const INNER_SIZE: usize = 8;

/// Given the worst case branching factor of `INNER_SIZE/2` = 4, this is the
/// worst case path length from the root node to a leaf node in a tree with 2^32
/// entries. We would run out of node references before we hit `MAX_PATH`.
const MAX_PATH: usize = 16;

/// Key comparator.
///
/// Keys don't need to implement `Ord`. They are compared using a comparator object which
/// provides a context for comparison.
pub trait Comparator<K>
where
    K: Copy,
{
    /// Compare keys `a` and `b`.
    ///
    /// This relation must provide a total ordering or the key space.
    fn cmp(&self, a: K, b: K) -> Ordering;

    /// Binary search for `k` in an ordered slice.
    ///
    /// Assume that `s` is already sorted according to this ordering, search for the key `k`.
    ///
    /// Returns `Ok(idx)` if `k` was found in the slice or `Err(idx)` with the position where it
    /// should be inserted to preserve the ordering.
    fn search(&self, k: K, s: &[K]) -> Result<usize, usize> {
        // TODO BENCHMARK branchless binary search
        s.binary_search_by(|x| self.cmp(*x, k))
    }
}

/// Trivial comparator that doesn't actually provide any context.
impl<K> Comparator<K> for ()
where
    K: Copy + Ord,
{
    fn cmp(&self, a: K, b: K) -> Ordering {
        a.cmp(&b)
    }
}

/// Family of types shared by the map and set forest implementations.
trait Forest {
    /// The key type is present for both sets and maps.
    type Key: Copy;

    /// The value type is `()` for sets.
    type Value: Copy;

    /// An array of keys for the leaf nodes.
    type LeafKeys: Copy + BorrowMut<[Self::Key]>;

    /// An array of values for the leaf nodes.
    type LeafValues: Copy + BorrowMut<[Self::Value]>;

    /// Splat a single key into a whole array.
    fn splat_key(key: Self::Key) -> Self::LeafKeys;

    /// Splat a single value inst a whole array
    fn splat_value(value: Self::Value) -> Self::LeafValues;
}

/// A reference to a B+-tree node.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Node(u32);
impl_idx_from!(Node(u32));

impl_debug_display!(match Node{
    Node(val) => "node{}", val;
});

/// Empty type to be used as the "value" in B-trees representing sets.
#[derive(Clone, Copy)]
struct SetValue();

/// Insert `x` into `s` at position `i`, pushing out the last element.
fn slice_insert<T: Copy>(s: &mut [T], i: usize, x: T) {
    for j in (i + 1..s.len()).rev() {
        s[j] = s[j - 1];
    }
    s[i] = x;
}

/// Shift elements in `s` to the left by `n` positions.
fn slice_shift<T: Copy>(s: &mut [T], n: usize) {
    for j in 0..s.len() - n {
        s[j] = s[j + n];
    }
}
