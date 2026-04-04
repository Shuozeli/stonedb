//! SkipList Implementation
//!
//! A SkipList is a probabilistic data structure that provides O(log n) average
//! complexity for insert, search, and delete operations.

use std::cmp::Ordering;

/// Maximum height for skip list nodes
const MAX_HEIGHT: usize = 20;

/// Probability of increasing height at each level
const BRANCHING: usize = 4;

/// A node in the skip list
pub struct Node<K, V> {
    pub key: K,
    pub value: V,
    _height: usize,
    forwards: Vec<*mut Node<K, V>>,
}

impl<K, V> Node<K, V> {
    fn new(key: K, value: V, height: usize) -> Box<Self> {
        let forwards: Vec<*mut Node<K, V>> = (0..height).map(|_| std::ptr::null_mut()).collect();
        Box::new(Self {
            key,
            value,
            _height: height,
            forwards,
        })
    }

    fn forward(&self, level: usize) -> Option<&Node<K, V>> {
        let ptr = self
            .forwards
            .get(level)
            .and_then(|p| if p.is_null() { None } else { Some(*p) })?;
        Some(unsafe { &*ptr })
    }
}

/// SkipList iterator
pub struct SkipListIterator<'a, K, V> {
    _list: &'a SkipList<K, V>,
    current: Option<&'a Node<K, V>>,
}

impl<'a, K, V> SkipListIterator<'a, K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    pub fn new(list: &'a SkipList<K, V>) -> Self {
        Self {
            _list: list,
            current: None,
        }
    }

    pub fn seek_to_first(&mut self) {
        self.current = self._list.head.forward(0);
    }

    /// Returns a reference to the current node, if valid.
    pub fn current(&self) -> Option<&'a Node<K, V>> {
        self.current
    }

    /// Seek to the first node with key >= the given key.
    pub fn seek(&mut self, key: &K) {
        let head_ptr = self._list.get_head_ptr();
        let mut current = head_ptr;

        for level in (0..self._list.max_height).rev() {
            unsafe {
                loop {
                    let next_ptr = (&(*current).forwards)[level];
                    if next_ptr.is_null() {
                        break;
                    }
                    let next_node = &*next_ptr;
                    match next_node.key.cmp(key) {
                        Ordering::Less => current = next_ptr,
                        Ordering::Greater | Ordering::Equal => {
                            if level == 0 {
                                self.current = Some(next_node);
                                return;
                            }
                            break;
                        }
                    }
                }
            }
        }
        self.current = None;
    }
}

impl<'a, K, V> Iterator for SkipListIterator<'a, K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    type Item = &'a Node<K, V>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.current {
            None => {
                self.current = self._list.head.forward(0);
            }
            Some(current) => {
                self.current = current.forward(0);
            }
        }
        self.current
    }
}

/// A SkipList implementing a sorted key-value store.
pub struct SkipList<K, V> {
    head: Box<Node<K, V>>,
    size: usize,
    max_height: usize,
    _marker: std::marker::PhantomData<(K, V)>,
}

impl<K, V> Drop for SkipList<K, V> {
    fn drop(&mut self) {
        // Free all nodes by traversing the bottom level and dropping them
        unsafe {
            let mut current = self.head.forwards[0];
            while !current.is_null() {
                let next = (&(*current).forwards)[0];
                let node = Box::from_raw(current);
                drop(node);
                current = next;
            }
        }
    }
}

impl<K, V> SkipList<K, V>
where
    K: Ord + Clone,
    V: Clone,
{
    /// Create a new empty SkipList with a sentinel head node.
    pub fn new() -> Self
    where
        K: Default,
        V: Default,
    {
        let head = Node::new(K::default(), V::default(), MAX_HEIGHT);
        Self {
            head,
            size: 0,
            max_height: 1,
            _marker: std::marker::PhantomData,
        }
    }

    /// Returns the number of elements in the list
    pub fn len(&self) -> usize {
        self.size
    }

    /// Returns true if the list is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the maximum height in the list
    pub fn max_height(&self) -> usize {
        self.max_height
    }

    fn get_head_ptr(&self) -> *mut Node<K, V> {
        &*self.head as *const Node<K, V> as *mut Node<K, V>
    }

    /// Insert a key-value pair into the list.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let top_level = random_height();
        let mut prevs: Vec<*mut Node<K, V>> = vec![std::ptr::null_mut(); top_level];

        let head_ptr = self.get_head_ptr();
        let mut current = head_ptr;

        // Search from top level down to find predecessors at each level
        // Note: we only need to search up to top_level levels because the new
        // node will only be inserted at levels < top_level. However, we must
        // start the search at each level from the correct position (the predecessor
        // found at the level above, not necessarily head).
        for level in (0..top_level).rev() {
            unsafe {
                loop {
                    let next_ptr = (&(*current).forwards)[level];
                    if next_ptr.is_null() {
                        prevs[level] = current;
                        break;
                    }
                    let next_node = &*next_ptr;
                    match next_node.key.cmp(&key) {
                        Ordering::Less => current = next_ptr,
                        Ordering::Greater | Ordering::Equal => {
                            prevs[level] = current;
                            break;
                        }
                    }
                }
            }
        }

        // Check for existing key
        let existing = unsafe {
            if !prevs[0].is_null() {
                let next_ptr = (&(*prevs[0]).forwards)[0];
                if !next_ptr.is_null() {
                    let next_node_mut = &mut (*next_ptr);
                    if next_node_mut.key == key {
                        let old_value = next_node_mut.value.clone();
                        next_node_mut.value = value;
                        return Some(old_value);
                    }
                }
            }
            None
        };

        // Create new node
        let new_node = Node::new(key, value, top_level);
        let new_ptr = Box::into_raw(new_node);

        // Insert at each level
        for level in 0..top_level {
            let prev_ptr = if level < prevs.len() && !prevs[level].is_null() {
                prevs[level]
            } else {
                head_ptr
            };

            unsafe {
                let next_ptr = (&(*prev_ptr).forwards)[level];
                (&mut (*new_ptr).forwards)[level] = next_ptr;
                (&mut (*prev_ptr).forwards)[level] = new_ptr;
            }
        }

        self.size += 1;
        if top_level > self.max_height {
            self.max_height = top_level;
        }

        existing
    }

    /// Search for a key, returning the value if found.
    pub fn get(&self, key: &K) -> Option<V> {
        let head_ptr = self.get_head_ptr();
        let mut current = head_ptr;

        for level in (0..self.max_height).rev() {
            unsafe {
                loop {
                    let next_ptr = (&(*current).forwards)[level];
                    if next_ptr.is_null() {
                        break;
                    }
                    let next_node = &*next_ptr;
                    match next_node.key.cmp(key) {
                        Ordering::Less => current = next_ptr,
                        Ordering::Greater => break,
                        Ordering::Equal => return Some(next_node.value.clone()),
                    }
                }
            }
        }
        None
    }

    /// Check if the list contains a key.
    pub fn contains(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Search for the first key >= the given key.
    pub fn lower_bound(&self, key: &K) -> Option<(K, V)>
    where
        K: Clone,
        V: Clone,
    {
        let head_ptr = self.get_head_ptr();
        let mut current = head_ptr;

        for level in (0..self.max_height).rev() {
            unsafe {
                loop {
                    let next_ptr = (&(*current).forwards)[level];
                    if next_ptr.is_null() {
                        break;
                    }
                    let next_node = &*next_ptr;
                    match next_node.key.cmp(key) {
                        Ordering::Less => current = next_ptr,
                        Ordering::Greater | Ordering::Equal => {
                            if level == 0 {
                                return Some((next_node.key.clone(), next_node.value.clone()));
                            }
                            break;
                        }
                    }
                }
            }
        }
        None
    }

    /// Returns an iterator over the list
    pub fn iter(&self) -> SkipListIterator<'_, K, V> {
        SkipListIterator::new(self)
    }
}

impl<K, V> Default for SkipList<K, V>
where
    K: Ord + Clone + Default,
    V: Clone + Default,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a random height for a new node.
fn random_height() -> usize {
    let mut height = 1;
    while height < MAX_HEIGHT && rand_level() {
        height += 1;
    }
    height
}

/// Returns true with probability 1/BRANCHING
fn rand_level() -> bool {
    let mut buf = [0u8; 1];
    getrandom::getrandom(&mut buf).unwrap_or_else(|_| panic!("failed to get random bytes"));
    (buf[0] as usize) < (256 / BRANCHING)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let mut list: SkipList<u64, u64> = SkipList::new();

        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        list.insert(1, 100);
        list.insert(2, 200);
        list.insert(3, 300);

        assert_eq!(list.len(), 3);
        assert!(!list.is_empty());

        assert_eq!(list.get(&1), Some(100));
        assert_eq!(list.get(&2), Some(200));
        assert_eq!(list.get(&3), Some(300));
        assert_eq!(list.get(&4), None);
    }

    #[test]
    fn test_update() {
        let mut list: SkipList<u64, u64> = SkipList::new();

        list.insert(1, 100);
        assert_eq!(list.get(&1), Some(100));

        list.insert(1, 101);
        assert_eq!(list.get(&1), Some(101));
    }

    #[test]
    fn test_contains() {
        let mut list: SkipList<u64, u64> = SkipList::new();

        list.insert(5, 500);

        assert!(!list.contains(&4));
        assert!(list.contains(&5));
        assert!(!list.contains(&6));
    }

    #[test]
    fn test_lower_bound() {
        let mut list: SkipList<u64, u64> = SkipList::new();

        list.insert(10, 1000);
        list.insert(30, 3000);
        list.insert(50, 5000);

        assert_eq!(list.lower_bound(&30), Some((30, 3000)));
        assert_eq!(list.lower_bound(&25), Some((30, 3000)));
        assert_eq!(list.lower_bound(&35), Some((50, 5000)));
        assert_eq!(list.lower_bound(&5), Some((10, 1000)));
        assert_eq!(list.lower_bound(&100), None);
    }

    #[test]
    fn test_string_keys() {
        let mut list: SkipList<String, String> = SkipList::new();

        list.insert("apple".into(), "red".into());
        list.insert("banana".into(), "yellow".into());

        assert_eq!(list.get(&"apple".into()), Some("red".into()));
        assert_eq!(list.get(&"banana".into()), Some("yellow".into()));
    }
}
