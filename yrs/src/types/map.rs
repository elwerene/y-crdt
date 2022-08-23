use crate::block::{Block, ItemContent, ItemPosition, Prelim};
use crate::event::Subscription;
use crate::types::{
    event_keys, Branch, BranchPtr, Entries, EntryChange, Observers, Path, Value, TYPE_REFS_MAP,
};
use crate::*;
use lib0::any::Any;
use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

/// Collection used to store key-value entries in an unordered manner. Keys are always represented
/// as UTF-8 strings. Values can be any value type supported by Yrs: JSON-like primitives as well as
/// shared data types.
///
/// In terms of conflict resolution, [Map] uses logical last-write-wins principle, meaning the past
/// updates are automatically overridden and discarded by newer ones, while concurrent updates made
/// by different peers are resolved into a single value using document id seniority to establish
/// order.
#[repr(transparent)]
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Map(BranchPtr);

impl Map {
    /// Converts all entries of a current map into JSON-like object representation.
    pub fn to_json(&self) -> Any {
        let inner = self.0;
        let mut res = HashMap::new();
        for (key, ptr) in inner.map.iter() {
            if let Block::Item(item) = ptr.deref() {
                if !item.is_deleted() {
                    let any = if let Some(value) = item.content.get_last() {
                        value.to_json()
                    } else {
                        Any::Null
                    };
                    res.insert(key.to_string(), any);
                }
            }
        }
        Any::Map(Box::new(res))
    }

    /// Returns a number of entries stored within current map.
    pub fn len(&self) -> u32 {
        let mut len = 0;
        let inner = self.0;
        for ptr in inner.map.values() {
            //TODO: maybe it would be better to just cache len in the map itself?
            if let Block::Item(item) = ptr.deref() {
                if !item.is_deleted() {
                    len += 1;
                }
            }
        }
        len
    }

    fn entries(&self) -> Entries {
        Entries::new(&self.0.map)
    }

    /// Returns an iterator that enables to traverse over all keys of entries stored within
    /// current map. These keys are not ordered.
    pub fn keys(&self) -> Keys {
        Keys(self.entries())
    }

    /// Returns an iterator that enables to traverse over all values stored within current map.
    pub fn values(&self) -> Values {
        Values(self.entries())
    }

    /// Returns an iterator that enables to traverse over all entries - tuple of key-value pairs -
    /// stored within current map.
    pub fn iter(&self) -> MapIter {
        MapIter(self.entries())
    }

    /// Inserts a new `value` under given `key` into current map. Returns a value stored previously
    /// under the same key (if any existed).
    pub fn insert<K: Into<Rc<str>>, V: Prelim>(
        &self,
        txn: &mut Transaction,
        key: K,
        value: V,
    ) -> Option<Value> {
        let key = key.into();
        let previous = self.get(&key);
        let pos = {
            let inner = self.0;
            let left = inner.map.get(&key);
            ItemPosition {
                parent: inner.into(),
                left: left.cloned(),
                right: None,
                index: 0,
                current_attrs: None,
            }
        };

        txn.create_item(&pos, value, Some(key));
        previous
    }

    /// Removes a stored within current map under a given `key`. Returns that value or `None` if
    /// no entry with a given `key` was present in current map.
    pub fn remove(&self, txn: &mut Transaction, key: &str) -> Option<Value> {
        self.0.remove(txn, key)
    }

    /// Returns a value stored under a given `key` within current map, or `None` if no entry
    /// with such `key` existed.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.0.get(key)
    }

    /// Checks if an entry with given `key` can be found within current map.
    pub fn contains(&self, key: &str) -> bool {
        if let Some(ptr) = self.0.map.get(key) {
            if let Block::Item(item) = ptr.deref() {
                return !item.is_deleted();
            }
        }
        false
    }

    /// Clears the contents of current map, effectively removing all of its entries.
    pub fn clear(&self, txn: &mut Transaction) {
        for (_, ptr) in self.0.map.iter() {
            txn.delete(ptr.clone());
        }
    }

    /// Subscribes a given callback to be triggered whenever current map is changed.
    /// A callback is triggered whenever a transaction gets committed. This function does not
    /// trigger if changes have been observed by nested shared collections.
    ///
    /// All map changes can be tracked by using [Event::keys] method.
    ///
    /// Returns an [Observer] which, when dropped, will unsubscribe current callback.
    pub fn observe<F>(&mut self, f: F) -> Subscription<MapEvent>
    where
        F: Fn(&Transaction, &MapEvent) -> () + 'static,
    {
        if let Observers::Map(eh) = self.0.observers.get_or_insert_with(Observers::map) {
            eh.subscribe(f)
        } else {
            panic!("Observed collection is of different type") //TODO: this should be Result::Err
        }
    }

    /// Unsubscribes a previously subscribed event callback identified by given `subscription_id`.
    pub fn unobserve(&mut self, subscription_id: SubscriptionId) {
        if let Some(Observers::Map(eh)) = self.0.observers.as_mut() {
            eh.unsubscribe(subscription_id);
        }
    }
}

impl AsRef<Branch> for Map {
    fn as_ref(&self) -> &Branch {
        self.0.deref()
    }
}

impl AsMut<Branch> for Map {
    fn as_mut(&mut self) -> &mut Branch {
        self.0.deref_mut()
    }
}

pub struct MapIter<'a>(Entries<'a>);

impl<'a> Iterator for MapIter<'a> {
    type Item = (&'a str, Value);

    fn next(&mut self) -> Option<Self::Item> {
        let (key, item) = self.0.next()?;
        if let Some(content) = item.content.get_last() {
            Some((key, content))
        } else {
            self.next()
        }
    }
}

/// An unordered iterator over the keys of a [Map].
pub struct Keys<'a>(Entries<'a>);

impl<'a> Iterator for Keys<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let (key, _) = self.0.next()?;
        Some(key)
    }
}

/// Iterator over the values of a [Map].
pub struct Values<'a>(Entries<'a>);

impl<'a> Iterator for Values<'a> {
    type Item = Vec<Value>;

    fn next(&mut self) -> Option<Self::Item> {
        let (_, item) = self.0.next()?;
        Some(item.content.get_content())
    }
}

impl From<BranchPtr> for Map {
    fn from(inner: BranchPtr) -> Self {
        Map(inner)
    }
}

/// A preliminary map. It can be used to early initialize the contents of a [Map], when it's about
/// to be inserted into another Yrs collection, such as [Array] or another [Map].
pub struct PrelimMap<T>(HashMap<String, T>);

impl<T> PrelimMap<T> {
    pub fn new() -> Self {
        PrelimMap(HashMap::default())
    }
}

impl<T> From<HashMap<String, T>> for PrelimMap<T> {
    fn from(map: HashMap<String, T>) -> Self {
        PrelimMap(map)
    }
}

impl<T: Prelim> Prelim for PrelimMap<T> {
    fn into_content(self, _txn: &mut Transaction) -> (ItemContent, Option<Self>) {
        let inner = Branch::new(TYPE_REFS_MAP, None);
        (ItemContent::Type(inner), Some(self))
    }

    fn integrate(self, txn: &mut Transaction, inner_ref: BranchPtr) {
        let map = Map::from(inner_ref);
        for (key, value) in self.0 {
            map.insert(txn, key, value);
        }
    }
}

/// Event generated by [Map::observe] method. Emitted during transaction commit phase.
pub struct MapEvent {
    pub current_target: BranchPtr,
    target: Map,
    keys: UnsafeCell<Result<HashMap<Rc<str>, EntryChange>, HashSet<Option<Rc<str>>>>>,
}

impl MapEvent {
    pub fn new(branch_ref: BranchPtr, key_changes: HashSet<Option<Rc<str>>>) -> Self {
        let current_target = branch_ref.clone();
        MapEvent {
            target: Map::from(branch_ref),
            current_target,
            keys: UnsafeCell::new(Err(key_changes)),
        }
    }

    /// Returns a [Map] instance which emitted this event.
    pub fn target(&self) -> &Map {
        &self.target
    }

    /// Returns a path from root type down to [Map] instance which emitted this event.
    pub fn path(&self) -> Path {
        Branch::path(self.current_target, self.target.0)
    }

    /// Returns a summary of key-value changes made over corresponding [Map] collection within
    /// bounds of current transaction.
    pub fn keys(&self, txn: &Transaction) -> &HashMap<Rc<str>, EntryChange> {
        let keys = unsafe { self.keys.get().as_mut().unwrap() };

        match keys {
            Ok(keys) => {
                return keys;
            }
            Err(subs) => {
                let subs = event_keys(txn, self.target.0, subs);
                *keys = Ok(subs);
                if let Ok(keys) = keys {
                    keys
                } else {
                    panic!("Defect: should not happen");
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::test_utils::{exchange_updates, run_scenario};
    use crate::types::text::PrelimText;
    use crate::types::{DeepObservable, EntryChange, Event, Map, Path, PathSegment, Value};
    use crate::updates::decoder::Decode;
    use crate::updates::encoder::{Encoder, EncoderV1};
    use crate::{Doc, PrelimArray, PrelimMap, StateVector, Update};
    use lib0::any::Any;
    use rand::distributions::Alphanumeric;
    use rand::prelude::{SliceRandom, StdRng};
    use rand::Rng;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ops::{Deref, DerefMut};
    use std::rc::Rc;

    #[test]
    fn map_basic() {
        let d1 = Doc::with_client_id(1);
        let mut t1 = d1.transact();
        let m1 = t1.get_map("map");

        let d2 = Doc::with_client_id(2);
        let mut t2 = d2.transact();
        let m2 = t2.get_map("map");

        m1.insert(&mut t1, "number".to_owned(), 1);
        m1.insert(&mut t1, "string".to_owned(), "hello Y");
        m1.insert(&mut t1, "object".to_owned(), {
            let mut v = HashMap::new();
            v.insert("key2".to_owned(), "value");

            let mut map = HashMap::new();
            map.insert("key".to_owned(), v);
            map // { key: { key2: 'value' } }
        });
        m1.insert(&mut t1, "boolean1".to_owned(), true);
        m1.insert(&mut t1, "boolean0".to_owned(), false);

        //let m1m = t1.get_map("y-map");
        //let m1a = t1.get_text("y-text");
        //m1a.insert(&mut t1, 0, "a");
        //m1a.insert(&mut t1, 0, "b");
        //m1m.insert(&mut t1, "y-text".to_owned(), m1a);

        //TODO: YArray within YMap
        fn compare_all(m: &Map) {
            assert_eq!(m.len(), 5);
            assert_eq!(m.get(&"number".to_owned()), Some(Value::from(1f64)));
            assert_eq!(m.get(&"boolean0".to_owned()), Some(Value::from(false)));
            assert_eq!(m.get(&"boolean1".to_owned()), Some(Value::from(true)));
            assert_eq!(m.get(&"string".to_owned()), Some(Value::from("hello Y")));
            assert_eq!(
                m.get(&"object".to_owned()),
                Some(Value::from({
                    let mut m = HashMap::new();
                    let mut n = HashMap::new();
                    n.insert("key2".to_owned(), Any::String("value".into()));
                    m.insert("key".to_owned(), Any::Map(Box::new(n)));
                    m
                }))
            );
        }

        compare_all(&m1);

        let update = d1.encode_state_as_update_v1(&StateVector::default());
        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap());

        compare_all(&m2);
    }

    #[test]
    fn map_get_set() {
        let d1 = Doc::with_client_id(1);
        let mut t1 = d1.transact();
        let m1 = t1.get_map("map");

        m1.insert(&mut t1, "stuff".to_owned(), "stuffy");
        m1.insert(&mut t1, "null".to_owned(), None as Option<String>);

        let update = d1.encode_state_as_update_v1(&StateVector::default());

        let d2 = Doc::with_client_id(2);
        let mut t2 = d2.transact();

        t2.apply_update(Update::decode_v1(update.as_slice()).unwrap());

        let m2 = t2.get_map("map");
        assert_eq!(m2.get(&"stuff".to_owned()), Some(Value::from("stuffy")));
        assert_eq!(m2.get(&"null".to_owned()), Some(Value::Any(Any::Null)));
    }

    #[test]
    fn map_get_set_sync_with_conflicts() {
        let d1 = Doc::with_client_id(1);
        let mut t1 = d1.transact();
        let m1 = t1.get_map("map");

        let d2 = Doc::with_client_id(2);
        let mut t2 = d2.transact();
        let m2 = t2.get_map("map");

        m1.insert(&mut t1, "stuff".to_owned(), "c0");
        m2.insert(&mut t2, "stuff".to_owned(), "c1");

        let u1 = d1.encode_state_as_update_v1(&StateVector::default());
        let u2 = d2.encode_state_as_update_v1(&StateVector::default());

        t1.apply_update(Update::decode_v1(u2.as_slice()).unwrap());
        t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap());

        assert_eq!(m1.get(&"stuff".to_owned()), Some(Value::from("c1")));
        assert_eq!(m2.get(&"stuff".to_owned()), Some(Value::from("c1")));
    }

    #[test]
    fn map_len_remove() {
        let d1 = Doc::with_client_id(1);
        let mut t1 = d1.transact();
        let m1 = t1.get_map("map");

        let key1 = "stuff".to_owned();
        let key2 = "other-stuff".to_owned();

        m1.insert(&mut t1, key1.clone(), "c0");
        m1.insert(&mut t1, key2.clone(), "c1");
        assert_eq!(m1.len(), 2);

        // remove 'stuff'
        assert_eq!(m1.remove(&mut t1, &key1), Some(Value::from("c0")));
        assert_eq!(m1.len(), 1);

        // remove 'stuff' again - nothing should happen
        assert_eq!(m1.remove(&mut t1, &key1), None);
        assert_eq!(m1.len(), 1);

        // remove 'other-stuff'
        assert_eq!(m1.remove(&mut t1, &key2), Some(Value::from("c1")));
        assert_eq!(m1.len(), 0);
    }

    #[test]
    fn map_clear() {
        let d1 = Doc::with_client_id(1);
        let mut t1 = d1.transact();
        let m1 = t1.get_map("map");

        m1.insert(&mut t1, "key1".to_owned(), "c0");
        m1.insert(&mut t1, "key2".to_owned(), "c1");
        m1.clear(&mut t1);

        assert_eq!(m1.len(), 0);
        assert_eq!(m1.get(&"key1".to_owned()), None);
        assert_eq!(m1.get(&"key2".to_owned()), None);

        let d2 = Doc::with_client_id(2);
        let mut t2 = d2.transact();

        let u1 = d1.encode_state_as_update_v1(&StateVector::default());
        t2.apply_update(Update::decode_v1(u1.as_slice()).unwrap());

        let m2 = t2.get_map("map");
        assert_eq!(m2.len(), 0);
        assert_eq!(m2.get(&"key1".to_owned()), None);
        assert_eq!(m2.get(&"key2".to_owned()), None);
    }

    #[test]
    fn map_clear_sync() {
        let d1 = Doc::with_client_id(1);
        let d2 = Doc::with_client_id(2);
        let d3 = Doc::with_client_id(3);
        let d4 = Doc::with_client_id(4);

        {
            let mut t1 = d1.transact();
            let mut t2 = d2.transact();
            let mut t3 = d3.transact();

            let m1 = t1.get_map("map");
            let m2 = t2.get_map("map");
            let m3 = t3.get_map("map");

            m1.insert(&mut t1, "key1".to_owned(), "c0");
            m2.insert(&mut t2, "key1".to_owned(), "c1");
            m2.insert(&mut t2, "key1".to_owned(), "c2");
            m3.insert(&mut t3, "key1".to_owned(), "c3");
        }

        exchange_updates(&[&d1, &d2, &d3, &d4]);

        {
            let mut t1 = d1.transact();
            let mut t2 = d2.transact();
            let mut t3 = d3.transact();

            let m1 = t1.get_map("map");
            let m2 = t2.get_map("map");
            let m3 = t3.get_map("map");

            m1.insert(&mut t1, "key2".to_owned(), "c0");
            m2.insert(&mut t2, "key2".to_owned(), "c1");
            m2.insert(&mut t2, "key2".to_owned(), "c2");
            m3.insert(&mut t3, "key2".to_owned(), "c3");
            m3.clear(&mut t3);
        }

        exchange_updates(&[&d1, &d2, &d3, &d4]);

        for doc in [d1, d2, d3, d4] {
            let mut txn = doc.transact();
            let map = txn.get_map("map");

            assert_eq!(
                map.get(&"key1".to_owned()),
                None,
                "'key1' entry for peer {} should be removed",
                doc.client_id
            );
            assert_eq!(
                map.get(&"key2".to_owned()),
                None,
                "'key2' entry for peer {} should be removed",
                doc.client_id
            );
            assert_eq!(
                map.len(),
                0,
                "all entries for peer {} should be removed",
                doc.client_id
            );
        }
    }

    #[test]
    fn map_get_set_with_3_way_conflicts() {
        let d1 = Doc::with_client_id(1);
        let d2 = Doc::with_client_id(2);
        let d3 = Doc::with_client_id(3);

        {
            let mut t1 = d1.transact();
            let mut t2 = d2.transact();
            let mut t3 = d3.transact();

            let m1 = t1.get_map("map");
            let m2 = t2.get_map("map");
            let m3 = t3.get_map("map");

            m1.insert(&mut t1, "stuff".to_owned(), "c0");
            m2.insert(&mut t2, "stuff".to_owned(), "c1");
            m2.insert(&mut t2, "stuff".to_owned(), "c2");
            m3.insert(&mut t3, "stuff".to_owned(), "c3");
        }

        exchange_updates(&[&d1, &d2, &d3]);

        for doc in [d1, d2, d3] {
            let mut txn = doc.transact();

            let map = txn.get_map("map");

            assert_eq!(
                map.get(&"stuff".to_owned()),
                Some(Value::from("c3")),
                "peer {} - map entry resolved to unexpected value",
                doc.client_id
            );
        }
    }

    #[test]
    fn map_get_set_remove_with_3_way_conflicts() {
        let d1 = Doc::with_client_id(1);
        let d2 = Doc::with_client_id(2);
        let d3 = Doc::with_client_id(3);
        let d4 = Doc::with_client_id(4);

        {
            let mut t1 = d1.transact();
            let mut t2 = d2.transact();
            let mut t3 = d3.transact();

            let m1 = t1.get_map("map");
            let m2 = t2.get_map("map");
            let m3 = t3.get_map("map");

            m1.insert(&mut t1, "key1".to_owned(), "c0");
            m2.insert(&mut t2, "key1".to_owned(), "c1");
            m2.insert(&mut t2, "key1".to_owned(), "c2");
            m3.insert(&mut t3, "key1".to_owned(), "c3");
        }

        exchange_updates(&[&d1, &d2, &d3, &d4]);

        {
            let mut t1 = d1.transact();
            let mut t2 = d2.transact();
            let mut t3 = d3.transact();
            let mut t4 = d4.transact();

            let m1 = t1.get_map("map");
            let m2 = t2.get_map("map");
            let m3 = t3.get_map("map");
            let m4 = t4.get_map("map");

            m1.insert(&mut t1, "key1".to_owned(), "deleteme");
            m2.insert(&mut t2, "key1".to_owned(), "c1");
            m3.insert(&mut t3, "key1".to_owned(), "c2");
            m4.insert(&mut t4, "key1".to_owned(), "c3");
            m4.remove(&mut t4, &"key1".to_owned());
        }

        exchange_updates(&[&d1, &d2, &d3, &d4]);

        for doc in [d1, d2, d3, d4] {
            let mut txn = doc.transact();
            let map = txn.get_map("map");

            assert_eq!(
                map.get(&"key1".to_owned()),
                None,
                "entry 'key1' on peer {} should be removed",
                doc.client_id
            );
        }
    }

    #[test]
    fn insert_and_remove_events() {
        let d1 = Doc::with_client_id(1);
        let mut m1 = {
            let mut txn = d1.transact();
            txn.get_map("map")
        };

        let entries = Rc::new(RefCell::new(None));
        let entries_c = entries.clone();
        let _sub = m1.observe(move |txn, e| {
            let keys = e.keys(txn);
            *entries_c.borrow_mut() = Some(keys.clone());
        });

        // insert new entry
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "a", 1);
            // txn is committed at the end of this scope
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "a".into(),
                EntryChange::Inserted(Any::Number(1.0).into())
            )]))
        );

        // update existing entry once
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "a", 2);
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "a".into(),
                EntryChange::Updated(Any::Number(1.0).into(), Any::Number(2.0).into())
            )]))
        );

        // update existing entry twice
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "a", 3);
            m1.insert(&mut txn, "a", 4);
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "a".into(),
                EntryChange::Updated(Any::Number(2.0).into(), Any::Number(4.0).into())
            )]))
        );

        // remove existing entry
        {
            let mut txn = d1.transact();
            m1.remove(&mut txn, "a");
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "a".into(),
                EntryChange::Removed(Any::Number(4.0).into())
            )]))
        );

        // add another entry and update it
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "b", 1);
            m1.insert(&mut txn, "b", 2);
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "b".into(),
                EntryChange::Inserted(Any::Number(2.0).into())
            )]))
        );

        // add and remove an entry
        {
            let mut txn = d1.transact();
            m1.insert(&mut txn, "c", 1);
            m1.remove(&mut txn, "c");
        }
        assert_eq!(entries.take(), Some(HashMap::new()));

        // copy updates over
        let d2 = Doc::with_client_id(2);
        let mut m2 = {
            let mut txn = d2.transact();
            txn.get_map("map")
        };

        let entries = Rc::new(RefCell::new(None));
        let entries_c = entries.clone();
        let _sub = m2.observe(move |txn, e| {
            let keys = e.keys(txn);
            *entries_c.borrow_mut() = Some(keys.clone());
        });

        {
            let t1 = d1.transact();
            let mut t2 = d2.transact();

            let sv = t2.state_vector();
            let mut encoder = EncoderV1::new();
            t1.encode_diff(&sv, &mut encoder);
            t2.apply_update(Update::decode_v1(encoder.to_vec().as_slice()).unwrap());
        }
        assert_eq!(
            entries.take(),
            Some(HashMap::from([(
                "b".into(),
                EntryChange::Inserted(Any::Number(2.0).into())
            )]))
        );
    }

    fn random_string(rng: &mut StdRng) -> String {
        let len = rng.gen_range(1, 10);
        rng.sample_iter(&Alphanumeric)
            .take(len)
            .map(char::from)
            .collect()
    }

    fn map_transactions() -> [Box<dyn Fn(&mut Doc, &mut StdRng)>; 3] {
        fn set(doc: &mut Doc, rng: &mut StdRng) {
            let mut txn = doc.transact();
            let map = txn.get_map("map");
            let key = ["one", "two"].choose(rng).unwrap();
            let value: String = random_string(rng);
            map.insert(&mut txn, key.to_string(), value);
        }

        fn set_type(doc: &mut Doc, rng: &mut StdRng) {
            let mut txn = doc.transact();
            let map = txn.get_map("map");
            let key = ["one", "two", "three"].choose(rng).unwrap();
            if rng.gen_bool(0.33) {
                map.insert(
                    &mut txn,
                    key.to_string(),
                    PrelimArray::from(vec![1, 2, 3, 4]),
                );
            } else if rng.gen_bool(0.33) {
                map.insert(&mut txn, key.to_string(), PrelimText("deeptext"));
            } else {
                map.insert(
                    &mut txn,
                    key.to_string(),
                    PrelimMap::from({
                        let mut map = HashMap::default();
                        map.insert("deepkey".to_owned(), "deepvalue");
                        map
                    }),
                );
            }
        }

        fn delete(doc: &mut Doc, rng: &mut StdRng) {
            let mut txn = doc.transact();
            let map = txn.get_map("map");
            let key = ["one", "two"].choose(rng).unwrap();
            map.remove(&mut txn, key);
        }
        [Box::new(set), Box::new(set_type), Box::new(delete)]
    }

    fn fuzzy(iterations: usize) {
        run_scenario(0, &map_transactions(), 5, iterations)
    }

    #[test]
    fn fuzzy_test_6() {
        fuzzy(6)
    }

    #[test]
    fn observe_deep() {
        let doc = Doc::with_client_id(1);
        let mut map = doc.transact().get_map("map");

        let paths = Rc::new(RefCell::new(vec![]));
        let calls = Rc::new(RefCell::new(0));
        let paths_copy = paths.clone();
        let calls_copy = calls.clone();
        let _sub = map.observe_deep(move |_txn, e| {
            let path: Vec<Path> = e.iter().map(Event::path).collect();
            paths_copy.borrow_mut().push(path);
            let mut count = calls_copy.borrow_mut();
            let count = count.deref_mut();
            *count += 1;
        });

        map.insert(&mut doc.transact(), "map", PrelimMap::<String>::new());
        let nested = map.get("map").unwrap().to_ymap().unwrap();
        nested.insert(
            &mut doc.transact(),
            "array",
            PrelimArray::from(Vec::<String>::default()),
        );
        let nested2 = nested.get("array").unwrap().to_yarray().unwrap();
        nested2.insert(&mut doc.transact(), 0, "content");

        nested.insert(&mut doc.transact(), "text", PrelimText("text"));
        let nested_text = nested.get("text").unwrap().to_ytext().unwrap();
        nested_text.push(&mut doc.transact(), "!");

        assert_eq!(*calls.borrow().deref(), 5);
        let actual = paths.borrow();
        assert_eq!(
            actual.as_slice(),
            &[
                vec![Path::from(vec![])],
                vec![Path::from(vec![PathSegment::Key("map".into())])],
                vec![Path::from(vec![
                    PathSegment::Key("map".into()),
                    PathSegment::Key("array".into())
                ])],
                vec![Path::from(vec![PathSegment::Key("map".into()),])],
                vec![Path::from(vec![
                    PathSegment::Key("map".into()),
                    PathSegment::Key("text".into()),
                ])],
            ]
        );
    }
}
