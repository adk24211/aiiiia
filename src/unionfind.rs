//! A union-find over [`Id`]s with path compression and union by size.

use crate::lang::Id;

#[derive(Clone, Debug, Default)]
pub struct UnionFind {
    parents: Vec<Id>,
    /// Size of each set, valid only at roots.
    sizes: Vec<u32>,
}

impl UnionFind {
    pub fn new() -> UnionFind {
        UnionFind::default()
    }

    /// Create a fresh singleton set and return its id.
    pub fn make_set(&mut self) -> Id {
        let id = Id::new(self.parents.len());
        self.parents.push(id);
        self.sizes.push(1);
        id
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.parents.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }

    /// The canonical representative of `id`, compressing the path walked.
    pub fn find(&mut self, mut id: Id) -> Id {
        while self.parents[id.index()] != id {
            // Path halving: point each node at its grandparent as we climb.
            let grandparent = self.parents[self.parents[id.index()].index()];
            self.parents[id.index()] = grandparent;
            id = grandparent;
        }
        id
    }

    /// The canonical representative of `id` without mutating anything.
    pub fn find_immutable(&self, mut id: Id) -> Id {
        while self.parents[id.index()] != id {
            id = self.parents[id.index()];
        }
        id
    }

    /// Merge the sets containing `a` and `b`.
    ///
    /// Returns `(root, merged_away)`: the surviving representative and the one
    /// that was absorbed. When `a` and `b` were already equal, both components
    /// are that same id.
    pub fn union(&mut self, a: Id, b: Id) -> (Id, Id) {
        let (mut ra, mut rb) = (self.find(a), self.find(b));
        if ra == rb {
            return (ra, ra);
        }
        // Union by size: the larger set keeps its representative.
        if self.sizes[ra.index()] < self.sizes[rb.index()] {
            std::mem::swap(&mut ra, &mut rb);
        }
        self.parents[rb.index()] = ra;
        self.sizes[ra.index()] += self.sizes[rb.index()];
        (ra, rb)
    }

    /// Number of distinct sets.
    pub fn set_count(&self) -> usize {
        (0..self.parents.len())
            .filter(|&i| self.parents[i] == Id::new(i))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> (UnionFind, Vec<Id>) {
        let mut uf = UnionFind::new();
        let v = (0..n).map(|_| uf.make_set()).collect();
        (uf, v)
    }

    #[test]
    fn singletons_are_their_own_roots() {
        let (mut uf, v) = ids(5);
        for &x in &v {
            assert_eq!(uf.find(x), x);
        }
        assert_eq!(uf.set_count(), 5);
    }

    #[test]
    fn union_is_transitive() {
        let (mut uf, v) = ids(6);
        uf.union(v[0], v[1]);
        uf.union(v[1], v[2]);
        uf.union(v[4], v[5]);
        assert_eq!(uf.find(v[0]), uf.find(v[2]));
        assert_eq!(uf.find(v[4]), uf.find(v[5]));
        assert_ne!(uf.find(v[0]), uf.find(v[4]));
        assert_ne!(uf.find(v[0]), uf.find(v[3]));
        assert_eq!(uf.set_count(), 3);
    }

    #[test]
    fn union_of_equal_is_a_noop() {
        let (mut uf, v) = ids(3);
        uf.union(v[0], v[1]);
        let before = uf.set_count();
        let (r, m) = uf.union(v[0], v[1]);
        assert_eq!(r, m);
        assert_eq!(uf.set_count(), before);
    }

    #[test]
    fn find_immutable_agrees_with_find() {
        let (mut uf, v) = ids(8);
        for i in 0..7 {
            uf.union(v[i], v[i + 1]);
        }
        for &x in &v {
            assert_eq!(uf.find_immutable(x), uf.find(x));
        }
        assert_eq!(uf.set_count(), 1);
    }

    #[test]
    fn long_chain_compresses() {
        let (mut uf, v) = ids(1000);
        for i in 0..999 {
            uf.union(v[i], v[i + 1]);
        }
        let root = uf.find(v[0]);
        for &x in &v {
            assert_eq!(uf.find(x), root);
        }
    }
}
