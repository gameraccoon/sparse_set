// Copyright (C) Pavel Grebnev 2024
// Distributed under the MIT License (license terms are at http://opensource.org/licenses/MIT).

mod sparse_entry;
mod sparse_key;
mod storage;

pub use sparse_key::SparseKey;

use sparse_entry::SparseEntry;
use sparse_entry::MAX_EPOCH;
use sparse_entry::MAX_SPARSE_INDEX;

/// A container based on Sparse Set, that stores a set of items and provides a way to efficiently
/// access them by a generated key.
///
/// Usage-wise it works similarly to an array, with exceptions that keys stay stable even after
/// removals, and operations like insertions and removals have slight overhead. Also, it has higher
/// memory consumption, since it needs to store additional data for each element.
///
/// Good for cache efficiency. Doesn't require any hashing. Can't be serialized.
///
/// Insertions are O(1) amortized.
/// Removals are O(1) if the order of elements can be changed, O(n) if the order must be preserved.
/// Accessing elements is O(1).
///
/// Extra memory consumption for each value is 4 * sizeof(usize) bytes on top of the size of the
/// value (e.g. 32 bytes per element on 64-bit systems).
/// The memory consumption will also grow by 2 * sizeof(usize) per 2^(sizeof(usize) * 8) elements
/// removed (e.g. 16 bytes per 18446744073709551616 elements removed on 64-bit systems).
///
/// ZST (zero-sized types) are not supported, trying to use them will result in a panic.
#[derive(Clone)]
pub struct SparseSet<T> {
    // storage of dense and sparse values
    storage: storage::SparseArrayStorage<T>,
    // a "free list" of free entries in the sparse array
    next_free_sparse_entry: usize,
}

#[allow(dead_code)]
impl<T> SparseSet<T> {
    /// Creates a new SparseSet. Does not allocate.
    ///
    /// # Panics
    ///
    /// Panics if the type `T` is zero-sized.
    pub fn new() -> Self {
        assert!(
            std::mem::size_of::<T>() > 0,
            "Zero-sized types are not supported"
        );
        Self {
            storage: storage::SparseArrayStorage::new(),
            next_free_sparse_entry: MAX_SPARSE_INDEX,
        }
    }

    /// Creates a new SparseSet with allocated memory for the given number of elements.
    ///
    /// # Panics
    ///
    /// - Panics if the type `T` is zero-sized.
    /// - Panics if the memory allocation fails.
    pub fn with_capacity(capacity: usize) -> Self {
        assert!(
            std::mem::size_of::<T>() > 0,
            "Zero-sized types are not supported"
        );
        Self {
            storage: storage::SparseArrayStorage::with_capacity(capacity),
            next_free_sparse_entry: MAX_SPARSE_INDEX,
        }
    }

    /// Inserts a new value into the set and returns a key that can be used to access it.
    ///
    /// This can heap-allocate (if the internal arrays need to grow) but it won't invalidate any
    /// existing keys.
    /// If some objects were removed before, it will reclaim the previously freed space.
    ///
    /// O(1) amortized time complexity.
    ///
    /// # Panics
    ///
    /// Panics if a memory allocation fails.
    pub fn push(&mut self, value: T) -> SparseKey {
        // if there are free entries in the sparse array, use one of them
        if self.next_free_sparse_entry != MAX_SPARSE_INDEX {
            let new_sparse_index = self.next_free_sparse_entry;
            let free_sparse_entry = self.storage.get_sparse()[new_sparse_index];
            self.next_free_sparse_entry = free_sparse_entry.next_free();

            let key = SparseKey {
                sparse_index: new_sparse_index,
                epoch: free_sparse_entry.next_epoch(),
            };

            self.storage.add_with_existing_sparse_item(key, value);

            key
        } else {
            // extend the sparse array
            self.storage.add_with_new_sparse_item(value)
        }
    }

    /// Removes an element from the set using the key, swapping it with the last element.
    /// Returns the removed value if it was present in the set.
    ///
    /// O(1) time complexity, however changes the order of elements.
    ///
    /// # Panics
    ///
    /// Can panic if the used key is not from this SparseSet.
    pub fn swap_remove(&mut self, key: SparseKey) -> Option<T> {
        // this can happen only if the key is from another SparseSet
        // in this case nothing is guaranteed anymore, we should panic
        assert!(key.sparse_index < self.storage.get_sparse_mut().len());

        let sparse_entry = self.storage.get_sparse_mut()[key.sparse_index];
        if sparse_entry.is_alive() && sparse_entry.epoch() == key.epoch {
            let swapped_sparse_index = self.storage.get_dense_keys()
                [self.storage.get_dense_values().len() - 1]
                .sparse_index;
            self.storage.get_sparse_mut()[swapped_sparse_index]
                .set_dense_index(sparse_entry.dense_index());

            let removed_value = self.storage.swap_remove_dense(sparse_entry.dense_index());

            self.mark_as_free(key, sparse_entry);
            Some(removed_value)
        } else {
            // the element was already removed (either there's nothing, or a newer element)
            None
        }
    }

    /// Removes an element from the set using the key, keeping the order of elements.
    /// Returns the removed value if it was present in the set.
    ///
    /// O(n) time complexity, however doesn't change the order of elements.
    ///
    /// # Panics
    ///
    /// Can panic if the used key is not from this SparseSet.
    pub fn remove(&mut self, key: SparseKey) -> Option<T> {
        // this can happen only if the key is from another SparseSet
        // in this case nothing is guaranteed anymore, we should panic
        assert!(key.sparse_index < self.storage.get_sparse().len());

        let sparse_entry = self.storage.get_sparse()[key.sparse_index];
        if sparse_entry.is_alive() && sparse_entry.epoch() == key.epoch {
            for i in sparse_entry.dense_index() + 1..self.storage.get_dense_values().len() {
                let sparse_index = self.storage.get_dense_keys()[i].sparse_index;

                self.storage.get_sparse_mut()[sparse_index].dense_index_move_left();
            }

            let removed_value = self.storage.remove_dense(sparse_entry.dense_index());

            self.mark_as_free(key, sparse_entry);
            Some(removed_value)
        } else {
            // the element was already removed (either there's nothing, or a newer element)
            None
        }
    }

    /// Swaps two elements in the set using their keys.
    ///
    /// O(1) time complexity.
    ///
    /// # Panics
    ///
    /// - Panics if any of the keys are not present in the set (were removed)
    /// - Can panic if the used keys are not from this SparseSet.
    pub fn swap(&mut self, key1: SparseKey, key2: SparseKey) {
        // this can happen only if the key is from another SparseSet
        // in this case nothing is guaranteed anymore, we should panic
        assert!(key1.sparse_index < self.storage.get_sparse().len());
        assert!(key2.sparse_index < self.storage.get_sparse().len());

        let sparse_entry1 = self.storage.get_sparse()[key1.sparse_index];
        let sparse_entry2 = self.storage.get_sparse()[key2.sparse_index];

        if sparse_entry1.is_alive() && sparse_entry2.is_alive() {
            self.storage
                .get_dense_values_mut()
                .swap(sparse_entry1.dense_index(), sparse_entry2.dense_index());
            self.storage
                .get_dense_keys_mut()
                .swap(sparse_entry1.dense_index(), sparse_entry2.dense_index());

            // swap the references in the sparse array
            let sparse_array = self.storage.get_sparse_mut();
            sparse_array[key1.sparse_index] =
                SparseEntry::new_alive(sparse_entry2.dense_index(), sparse_entry1.epoch());
            sparse_array[key2.sparse_index] =
                SparseEntry::new_alive(sparse_entry1.dense_index(), sparse_entry2.epoch());
        } else {
            panic!("Cannot swap elements that are not alive");
        }
    }

    /// Returns a reference to the value stored at the given key.
    /// If the key is not valid, returns None.
    ///
    /// O(1) time complexity.
    ///
    /// # Panics
    ///
    /// Can panic if the used key is not from this SparseSet.
    pub fn get(&self, key: SparseKey) -> Option<&T> {
        // this can happen only if the key is from another SparseSet
        // in this case nothing is guaranteed anymore, we should panic
        assert!(key.sparse_index < self.storage.get_sparse().len());

        let sparse_entry = self.storage.get_sparse()[key.sparse_index];
        if sparse_entry.is_alive() && sparse_entry.epoch() == key.epoch {
            Some(&self.storage.get_dense_values()[sparse_entry.dense_index()])
        } else {
            // either there's no element, or there's a newer element the value points to
            None
        }
    }

    /// Returns a mutable reference to the value stored at the given key.
    /// If the key is not valid, returns None.
    ///
    /// O(1) time complexity.
    ///
    /// # Panics
    ///
    /// Can panic if the used key is not from this SparseSet.
    pub fn get_mut(&mut self, key: SparseKey) -> Option<&mut T> {
        // this can happen only if the key is from another SparseSet
        // in this case nothing is guaranteed anymore, we should panic
        assert!(key.sparse_index < self.storage.get_sparse().len());

        let sparse_entry = self.storage.get_sparse()[key.sparse_index];

        if sparse_entry.is_alive() && sparse_entry.epoch() == key.epoch {
            Some(&mut self.storage.get_dense_values_mut()[sparse_entry.dense_index()])
        } else {
            // either there's no element, or there's a newer element the value points to
            None
        }
    }

    /// Returns true if the key points to a valid element in the set.
    ///
    /// O(1) time complexity.
    ///
    /// # Panics
    ///
    /// Can panic if the used key is not from this SparseSet.
    pub fn contains(&self, key: SparseKey) -> bool {
        if key.sparse_index >= self.storage.get_sparse().len() {
            debug_assert!(false, "The key is not valid for this SparseSet");
            return false;
        }

        let sparse_entry = self.storage.get_sparse()[key.sparse_index];
        return sparse_entry.is_alive() && sparse_entry.epoch() == key.epoch;
    }

    /// Returns the number of elements in the set.
    ///
    /// O(1) time complexity.
    pub fn size(&self) -> usize {
        self.storage.get_dense_values().len()
    }

    /// Returns true if the set is empty.
    ///
    /// O(1) time complexity.
    pub fn is_empty(&self) -> bool {
        self.storage.get_dense_values().is_empty()
    }

    /// Returns an iterator over the values of the set.
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &T> {
        self.storage.get_dense_values().iter()
    }

    /// Returns an iterator over the mutable values of the set.
    pub fn values_mut(&mut self) -> impl DoubleEndedIterator<Item = &mut T> {
        self.storage.get_dense_values_mut().iter_mut()
    }

    /// Returns an iterator over the keys of the set.
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = SparseKey> + '_ {
        self.storage.get_dense_keys().iter().copied()
    }

    /// Returns an iterator over the key-value pairs of the set.
    ///
    /// Note that if you intend to iterate over key-values in time-critical code, it may be better
    /// to instead store the keys in the elements themselves to reduce CPU cache misses.
    pub fn key_values(&self) -> impl DoubleEndedIterator<Item = (SparseKey, &T)> {
        self.storage
            .get_dense_keys()
            .iter()
            .copied()
            .zip(self.storage.get_dense_values().iter())
    }

    fn mark_as_free(&mut self, key: SparseKey, entry: SparseEntry) {
        self.storage.get_sparse_mut()[key.sparse_index] = SparseEntry::new_free(
            self.next_free_sparse_entry,
            usize::wrapping_add(entry.epoch(), 1),
        );

        // as long as we have available epochs, we can reuse the sparse entry
        if key.epoch < MAX_EPOCH {
            self.next_free_sparse_entry = key.sparse_index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // empty sparse set => created => no items
    #[test]
    fn empty_sparse_set_created_no_items() {
        let sparse_set: SparseSet<i32> = SparseSet::new();

        assert_eq!(sparse_set.size(), 0);
        for _ in sparse_set.values() {
            assert!(false);
        }
    }

    // empty sparse set => created with capacity => no items
    #[test]
    fn empty_sparse_set_created_with_capacity_no_items() {
        let sparse_set: SparseSet<i32> = SparseSet::with_capacity(10);

        assert_eq!(sparse_set.size(), 0);
        for _ in sparse_set.values() {
            assert!(false);
        }
    }

    // empty sparse set => push item => has one item
    #[test]
    fn empty_sparse_set_push_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();

        let key = sparse_set.push(42);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&42));
    }

    // empty sparse set with capacity => push item => has one item
    #[test]
    fn empty_sparse_set_with_capacity_push_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::with_capacity(10);

        let key = sparse_set.push(42);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&42));
    }

    // sparse set with one item => mutate the item => the item is changed
    #[test]
    fn sparse_set_with_one_item_mutate_the_item_the_item_is_changed() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        *sparse_set.get_mut(key).unwrap() = 43;

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&43));
    }

    // sparse set with one item => remove item => no items
    #[test]
    fn sparse_set_with_one_item_remove_item_no_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set with one item => swap_remove item => no items
    #[test]
    fn swap_sparse_set_with_one_item_remove_item_no_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.swap_remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set with one item => add second item and remove first item => only second item remains
    #[test]
    fn sparse_set_with_one_item_add_second_item_and_remove_first_item_only_second_item_remains() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.remove(key1);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&43));
    }

    // sparse set with one item => remove item and add two new items => has two items
    #[test]
    fn sparse_set_with_one_item_remove_item_and_add_two_new_items_has_two_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);

        sparse_set.remove(key1);
        let key2 = sparse_set.push(43);
        let key3 = sparse_set.push(44);

        assert_eq!(sparse_set.size(), 2);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&43));
        assert_eq!(sparse_set.get(key3), Some(&44));
    }

    // sparse set with two items => remove first item => has one item
    #[test]
    fn sparse_set_with_two_items_remove_first_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.remove(key1);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&43));
    }

    // sparse set with two items => swap_remove first item => has one item
    #[test]
    fn swap_sparse_set_with_two_items_remove_first_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.swap_remove(key1);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&43));
    }

    // sparse set with two items => remove second item => has one item
    #[test]
    fn sparse_set_with_two_items_remove_second_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.remove(key2);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), Some(&42));
        assert_eq!(sparse_set.get(key2), None);
    }

    // sparse set with two items => swap_remove second item => has one item
    #[test]
    fn swap_sparse_set_with_two_items_remove_second_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.swap_remove(key2);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), Some(&42));
        assert_eq!(sparse_set.get(key2), None);
    }

    // spare set with one item => remove an item and push new item => has one item
    #[test]
    fn sparse_set_with_one_item_remove_an_item_and_push_new_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);
        sparse_set.remove(key);

        let new_key = sparse_set.push(43);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), None);
        assert_eq!(sparse_set.get(new_key), Some(&43));
    }

    // sparse set with one item => swap_remove an item and push new item => has one item
    #[test]
    fn swap_sparse_set_with_one_item_remove_an_item_and_push_new_item_has_one_item() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);
        sparse_set.swap_remove(key);

        let new_key = sparse_set.push(43);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), None);
        assert_eq!(sparse_set.get(new_key), Some(&43));
    }

    // sparse set with five items => remove first item => order is not changed
    #[test]
    fn sparse_set_with_five_items_remove_first_item_order_is_not_changed() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);
        let key3 = sparse_set.push(44);
        let key4 = sparse_set.push(45);
        let key5 = sparse_set.push(46);

        sparse_set.remove(key1);

        assert_eq!(sparse_set.size(), 4);
        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &43);
            } else if i == 1 {
                assert_eq!(value, &44);
            } else if i == 2 {
                assert_eq!(value, &45);
            } else {
                assert_eq!(value, &46);
            }
        }
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&43));
        assert_eq!(sparse_set.get(key3), Some(&44));
        assert_eq!(sparse_set.get(key4), Some(&45));
        assert_eq!(sparse_set.get(key5), Some(&46));
    }

    // sparse set with one item => remove item twice => no items
    #[test]
    fn sparse_set_with_one_item_remove_item_twice_no_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.remove(key);
        sparse_set.remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set with one item => remove item twice => no items
    #[test]
    fn sparse_set_with_one_item_swap_remove_item_twice_no_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.swap_remove(key);
        sparse_set.swap_remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set with three items => iterate over values => the values are iterated in order
    #[test]
    fn sparse_set_with_three_items_iterate_over_values_the_values_are_iterated_in_order() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        sparse_set.push(42);
        sparse_set.push(43);
        sparse_set.push(44);

        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &42);
            } else if i == 1 {
                assert_eq!(value, &43);
            } else {
                assert_eq!(value, &44);
            }
        }
    }

    // sparse set with three items => iterate over keys => the keys are iterated in order
    #[test]
    fn sparse_set_with_three_items_iterate_over_keys_the_keys_are_iterated_in_order() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        sparse_set.push(42);
        sparse_set.push(43);
        sparse_set.push(44);

        for (i, key) in sparse_set.keys().enumerate() {
            if i == 0 {
                assert_eq!(sparse_set.get(key), Some(&42));
            } else if i == 1 {
                assert_eq!(sparse_set.get(key), Some(&43));
            } else {
                assert_eq!(sparse_set.get(key), Some(&44));
            }
        }
    }

    // sparse set with three items => iterate over key-values => the key-values are iterated in order
    #[test]
    fn sparse_set_with_three_items_iterate_over_key_values_the_key_values_are_iterated_in_order() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);
        let key3 = sparse_set.push(44);

        for (i, (key, value)) in sparse_set.key_values().enumerate() {
            if i == 0 {
                assert_eq!(value, &42);
                assert_eq!(key, key1);
            } else if i == 1 {
                assert_eq!(value, &43);
                assert_eq!(key, key2);
            } else {
                assert_eq!(value, &44);
                assert_eq!(key, key3);
            }
        }
    }

    // sparse set with one item => iterate over values and mutate => the value is changed
    #[test]
    fn sparse_set_with_one_item_iterate_over_values_and_mutate_the_value_is_changed() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        for value in sparse_set.values_mut() {
            *value = 43;
        }

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&43));
    }

    // sparse set with two items => swap the items => the items are swapped in order but not by keys
    #[test]
    fn sparse_set_with_two_items_swap_the_items_the_items_are_swapped_in_order_but_not_by_keys() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.swap(key1, key2);

        assert_eq!(sparse_set.size(), 2);
        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &43);
            } else {
                assert_eq!(value, &42);
            }
        }
        assert_eq!(sparse_set.get(key1), Some(&42));
        assert_eq!(sparse_set.get(key2), Some(&43));
    }

    // sparse set with one item => try swapping with itself => does nothing
    #[test]
    fn sparse_set_with_one_item_try_swapping_with_itself_does_nothing() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.swap(key, key);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&42));
    }

    // sparse set with five items => clone the set => cloned set has the same items
    #[test]
    fn sparse_set_with_five_items_clone_the_set_cloned_set_has_the_same_items() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);
        let key3 = sparse_set.push(44);
        let key4 = sparse_set.push(45);
        let key5 = sparse_set.push(46);

        let cloned_sparse_set = sparse_set.clone();

        assert_eq!(cloned_sparse_set.size(), 5);
        assert_eq!(cloned_sparse_set.get(key1), Some(&42));
        assert_eq!(cloned_sparse_set.get(key2), Some(&43));
        assert_eq!(cloned_sparse_set.get(key3), Some(&44));
        assert_eq!(cloned_sparse_set.get(key4), Some(&45));
        assert_eq!(cloned_sparse_set.get(key5), Some(&46));
    }

    // sparse set with one item => check if contains => returns true
    #[test]
    fn sparse_set_with_one_item_check_if_contains_returns_true() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        assert!(sparse_set.contains(key));
    }

    // sparse set with one item => remove the item and check if contains => returns false
    #[test]
    fn sparse_set_with_one_item_remove_the_item_and_check_if_contains_returns_false() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key = sparse_set.push(42);

        sparse_set.swap_remove(key);

        assert!(!sparse_set.contains(key));
    }

    // sparse set with two items => remove item and try to swap it => panics
    #[test]
    #[should_panic]
    fn sparse_set_with_two_items_remove_item_and_try_to_swap_it_panics() {
        let mut sparse_set: SparseSet<i32> = SparseSet::new();
        let key1 = sparse_set.push(42);
        let key2 = sparse_set.push(43);

        sparse_set.remove(key1);
        sparse_set.swap(key1, key2);
    }

    // two sparse sets with different sizes => try to access non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_with_different_sizes_try_to_access_non_existent_key_panics() {
        let mut sparse_set1: SparseSet<i32> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push(42);

        let sparse_set2: SparseSet<i32> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.get(key1);
    }

    // two sparse sets with different sizes => try to remove non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_with_different_sizes_try_to_remove_non_existent_key_panics() {
        let mut sparse_set1: SparseSet<i32> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push(42);

        let mut sparse_set2: SparseSet<i32> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.remove(key1);
    }

    // two sparse sets with different sizes => try to swap_remove non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_with_different_sizes_try_to_swap_remove_non_existent_key_panics() {
        let mut sparse_set1: SparseSet<i32> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push(42);

        let mut sparse_set2: SparseSet<i32> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.swap_remove(key1);
    }

    // two sparse sets with different sizes => try to swap non-existent keys => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_with_different_sizes_try_to_swap_non_existent_keys_panics() {
        let mut sparse_set1: SparseSet<i32> = SparseSet::with_capacity(1);
        sparse_set1.push(42);
        let key2 = sparse_set1.push(43);

        let mut sparse_set2: SparseSet<i32> = SparseSet::new();
        let key3 = sparse_set2.push(44);

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.swap(key2, key3);
    }

    // empty sparse set of strings => created => no items
    #[test]
    fn empty_sparse_set_of_strings_created_no_items() {
        let sparse_set: SparseSet<String> = SparseSet::new();

        assert_eq!(sparse_set.size(), 0);
        for _ in sparse_set.values() {
            assert!(false);
        }
    }

    // empty sparse set of strings => created with capacity => no items
    #[test]
    fn empty_sparse_set_of_strings_created_with_capacity_no_items() {
        let sparse_set: SparseSet<String> = SparseSet::with_capacity(10);

        assert_eq!(sparse_set.size(), 0);
        for _ in sparse_set.values() {
            assert!(false);
        }
    }

    // empty sparse set of strings => push item => has one item
    #[test]
    fn empty_sparse_set_of_strings_push_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let expected = "42".to_string();

        let key = sparse_set.push("42".to_string());

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&expected));
    }

    // empty sparse set of strings with capacity => push item => has one item
    #[test]
    fn empty_sparse_set_of_strings_with_capacity_push_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::with_capacity(10);

        let key = sparse_set.push("42".to_string());

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&"42".to_string()));
    }

    // sparse set of strings with one item => mutate the item => the item is changed
    #[test]
    fn sparse_set_of_strings_with_one_item_mutate_the_item_the_item_is_changed() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        *sparse_set.get_mut(key).unwrap() = "43".to_string();

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&"43".to_string()));
    }

    // sparse set of strings with one item => remove item => no items
    #[test]
    fn sparse_set_of_strings_with_one_item_remove_item_no_items() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set of strings with one item => swap_remove item => no items
    #[test]
    fn swap_sparse_set_of_strings_with_one_item_remove_item_no_items() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.swap_remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set of strings with two items => remove first item => has one item
    #[test]
    fn sparse_set_of_strings_with_two_items_remove_first_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.remove(key1);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&"43".to_string()));
    }

    // sparse set of strings with two items => swap_remove first item => has one item
    #[test]
    fn swap_sparse_set_of_strings_with_two_items_remove_first_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.swap_remove(key1);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&"43".to_string()));
    }

    // sparse set of strings with two items => remove second item => has one item
    #[test]
    fn sparse_set_of_strings_with_two_items_remove_second_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.remove(key2);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), Some(&"42".to_string()));
        assert_eq!(sparse_set.get(key2), None);
    }

    // sparse set of strings with two items => swap_remove second item => has one item
    #[test]
    fn swap_sparse_set_of_strings_with_two_items_remove_second_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.swap_remove(key2);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key1), Some(&"42".to_string()));
        assert_eq!(sparse_set.get(key2), None);
    }

    // spare set of strings with one item => remove an item and push new item => has one item
    #[test]
    fn sparse_set_of_strings_with_one_item_remove_an_item_and_push_new_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());
        sparse_set.remove(key);

        let new_key = sparse_set.push("43".to_string());

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), None);
        assert_eq!(sparse_set.get(new_key), Some(&"43".to_string()));
    }

    // sparse set of strings with one item => swap_remove an item and push new item => has one item
    #[test]
    fn swap_sparse_set_of_strings_with_one_item_remove_an_item_and_push_new_item_has_one_item() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());
        sparse_set.swap_remove(key);

        let new_key = sparse_set.push("43".to_string());

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), None);
        assert_eq!(sparse_set.get(new_key), Some(&"43".to_string()));
    }

    // sparse set of strings with five items => remove first item => order is not changed
    #[test]
    fn sparse_set_of_strings_with_five_items_remove_first_item_order_is_not_changed() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());
        let key3 = sparse_set.push("44".to_string());
        let key4 = sparse_set.push("45".to_string());
        let key5 = sparse_set.push("46".to_string());

        sparse_set.remove(key1);

        assert_eq!(sparse_set.size(), 4);
        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &"43".to_string());
            } else if i == 1 {
                assert_eq!(value, &"44".to_string());
            } else if i == 2 {
                assert_eq!(value, &"45".to_string());
            } else {
                assert_eq!(value, &"46".to_string());
            }
        }
        assert_eq!(sparse_set.get(key1), None);
        assert_eq!(sparse_set.get(key2), Some(&"43".to_string()));
        assert_eq!(sparse_set.get(key3), Some(&"44".to_string()));
        assert_eq!(sparse_set.get(key4), Some(&"45".to_string()));
        assert_eq!(sparse_set.get(key5), Some(&"46".to_string()));
    }

    // sparse set of strings with one item => remove item twice => no items
    #[test]
    fn sparse_set_of_strings_with_one_item_remove_item_twice_no_items() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.remove(key);
        sparse_set.remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set of strings with one item => remove item twice => no items
    #[test]
    fn sparse_set_of_strings_with_one_item_swap_remove_item_twice_no_items() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.swap_remove(key);
        sparse_set.swap_remove(key);

        assert_eq!(sparse_set.size(), 0);
        assert_eq!(sparse_set.get(key), None);
    }

    // sparse set of strings with three items => iterate over values => the values are iterated in order
    #[test]
    fn sparse_set_of_strings_with_three_items_iterate_over_values_the_values_are_iterated_in_order()
    {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        sparse_set.push("42".to_string());
        sparse_set.push("43".to_string());
        sparse_set.push("44".to_string());

        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &"42".to_string());
            } else if i == 1 {
                assert_eq!(value, &"43".to_string());
            } else {
                assert_eq!(value, &"44".to_string());
            }
        }
    }

    // sparse set of strings with three items => iterate over keys => the keys are iterated in order
    #[test]
    fn sparse_set_of_strings_with_three_items_iterate_over_keys_the_keys_are_iterated_in_order() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        sparse_set.push("42".to_string());
        sparse_set.push("43".to_string());
        sparse_set.push("44".to_string());

        for (i, key) in sparse_set.keys().enumerate() {
            let expected = if i == 0 {
                "42".to_string()
            } else if i == 1 {
                "43".to_string()
            } else {
                "44".to_string()
            };

            assert_eq!(sparse_set.get(key), Some(&expected));
        }
    }

    // sparse set of strings with three items => iterate over key-values => the key-values are iterated in order
    #[test]
    fn sparse_set_of_strings_with_three_items_iterate_over_key_values_the_key_values_are_iterated_in_order(
    ) {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());
        let key3 = sparse_set.push("44".to_string());

        for (i, (key, value)) in sparse_set.key_values().enumerate() {
            if i == 0 {
                assert_eq!(value, &"42".to_string());
                assert_eq!(key, key1);
            } else if i == 1 {
                assert_eq!(value, &"43".to_string());
                assert_eq!(key, key2);
            } else {
                assert_eq!(value, &"44".to_string());
                assert_eq!(key, key3);
            }
        }
    }

    // sparse set of strings with one item => iterate over values and mutate => the value is changed
    #[test]
    fn sparse_set_of_strings_with_one_item_iterate_over_values_and_mutate_the_value_is_changed() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        for value in sparse_set.values_mut() {
            *value = "43".to_string();
        }

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&"43".to_string()));
    }

    // sparse set of strings with two items => swap the items => the items are swapped in order but not by keys
    #[test]
    fn sparse_set_of_strings_with_two_items_swap_the_items_the_items_are_swapped_in_order_but_not_by_keys(
    ) {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.swap(key1, key2);

        assert_eq!(sparse_set.size(), 2);
        for (i, value) in sparse_set.values().enumerate() {
            if i == 0 {
                assert_eq!(value, &"43".to_string());
            } else {
                assert_eq!(value, &"42".to_string());
            }
        }
        assert_eq!(sparse_set.get(key1), Some(&"42".to_string()));
        assert_eq!(sparse_set.get(key2), Some(&"43".to_string()));
    }

    // sparse set of strings with one item => try swapping with itself => does nothing
    #[test]
    fn sparse_set_of_strings_with_one_item_try_swapping_with_itself_does_nothing() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.swap(key, key);

        assert_eq!(sparse_set.size(), 1);
        assert_eq!(sparse_set.get(key), Some(&"42".to_string()));
    }

    // sparse set of strings with five items => clone the set => cloned set has the same items
    #[test]
    fn sparse_set_of_strings_with_five_items_clone_the_set_cloned_set_has_the_same_items() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());
        let key3 = sparse_set.push("44".to_string());
        let key4 = sparse_set.push("45".to_string());
        let key5 = sparse_set.push("46".to_string());

        let cloned_sparse_set = sparse_set.clone();

        assert_eq!(cloned_sparse_set.size(), 5);
        assert_eq!(cloned_sparse_set.get(key1), Some(&"42".to_string()));
        assert_eq!(cloned_sparse_set.get(key2), Some(&"43".to_string()));
        assert_eq!(cloned_sparse_set.get(key3), Some(&"44".to_string()));
        assert_eq!(cloned_sparse_set.get(key4), Some(&"45".to_string()));
        assert_eq!(cloned_sparse_set.get(key5), Some(&"46".to_string()));
    }

    // sparse set of strings with one item => check if contains => returns true
    #[test]
    fn sparse_set_of_strings_with_one_item_check_if_contains_returns_true() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        assert!(sparse_set.contains(key));
    }

    // sparse set of strings with one item => remove the item and check if contains => returns false
    #[test]
    fn sparse_set_of_strings_with_one_item_remove_the_item_and_check_if_contains_returns_false() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key = sparse_set.push("42".to_string());

        sparse_set.swap_remove(key);

        assert!(!sparse_set.contains(key));
    }

    // sparse set with two strings => remove item and try to swap it => panics
    #[test]
    #[should_panic]
    fn sparse_set_with_two_strings_remove_item_and_try_to_swap_it_panics() {
        let mut sparse_set: SparseSet<String> = SparseSet::new();
        let key1 = sparse_set.push("42".to_string());
        let key2 = sparse_set.push("43".to_string());

        sparse_set.remove(key1);
        sparse_set.swap(key1, key2);
    }

    // two sparse sets of strings with different sizes => try to access non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_of_strings_with_different_sizes_try_to_access_non_existent_key_panics() {
        let mut sparse_set1: SparseSet<String> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push("42".to_string());

        let sparse_set2: SparseSet<String> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.get(key1);
    }

    // two sparse sets of strings with different sizes => try to remove non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_of_strings_with_different_sizes_try_to_remove_non_existent_key_panics() {
        let mut sparse_set1: SparseSet<String> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push("42".to_string());

        let mut sparse_set2: SparseSet<String> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.remove(key1);
    }

    // two sparse sets of strings with different sizes => try to swap_remove non-existent key => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_of_strings_with_different_sizes_try_to_swap_remove_non_existent_key_panics()
    {
        let mut sparse_set1: SparseSet<String> = SparseSet::with_capacity(1);
        let key1 = sparse_set1.push("42".to_string());

        let mut sparse_set2: SparseSet<String> = SparseSet::new();

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.swap_remove(key1);
    }

    // two sparse sets of strings with different sizes => try to swap non-existent keys => panics
    #[test]
    #[should_panic]
    fn two_sparse_sets_of_strings_with_different_sizes_try_to_swap_non_existent_keys_panics() {
        let mut sparse_set1: SparseSet<String> = SparseSet::with_capacity(1);
        sparse_set1.push("42".to_string());
        let key2 = sparse_set1.push("43".to_string());

        let mut sparse_set2: SparseSet<String> = SparseSet::new();
        let key3 = sparse_set2.push("44".to_string());

        // in this specific case it will panic to prevent UB
        // however, in general, it's not guaranteed to panic
        sparse_set2.swap(key2, key3);
    }

    // sparse set with ZST => try to create => panics
    #[test]
    #[should_panic]
    fn sparse_set_with_zst_try_to_create_panics() {
        let _sparse_set: SparseSet<()> = SparseSet::new();
    }

    // sparse set with ZST => try to create with capacity => panics
    #[test]
    #[should_panic]
    fn sparse_set_with_zst_try_to_create_with_capacity_panics() {
        let _sparse_set: SparseSet<()> = SparseSet::with_capacity(10);
    }

    // sparse set of static string => try to pass as a value with more generic lifetime => compiles
    #[test]
    fn sparse_set_of_static_string_try_to_pass_as_a_value_with_more_generic_lifetime_compiles() {
        #[allow(clippy::needless_lifetimes)]
        fn accepting_sparse_set_of_string_with_lifetime<'a>(_sparse_set: &SparseSet<&'a str>) {}

        let sparse_set: SparseSet<&'static str> = SparseSet::new();
        accepting_sparse_set_of_string_with_lifetime(&sparse_set);
    }

    // sparse set => is send => true
    #[test]
    fn sparse_set_is_send() {
        fn is_send<T: Send>() {}
        is_send::<SparseSet<i32>>();
    }

    // sparse set => is sync => true
    #[test]
    fn sparse_set_is_sync() {
        fn is_sync<T: Sync>() {}
        is_sync::<SparseSet<i32>>();
    }
}
