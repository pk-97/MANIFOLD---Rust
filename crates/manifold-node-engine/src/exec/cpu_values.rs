//! Typed CPU values that follow graph resource slots through one evaluation.
//!
//! The public API stays generic while the executor and backends keep one
//! erased table per registered payload type. Values remain inline in their
//! typed maps; erasure is limited to the maps and write buffers themselves.

use std::any::{Any, TypeId, type_name};

use ahash::AHashMap;

use crate::bindings::Slot;

trait ErasedValues: Send {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn remove(&mut self, slot: Slot);
    fn clear(&mut self);
    fn commit(&mut self, writes: &mut dyn ErasedWrites);
}

trait ErasedWrites: Send {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn clear(&mut self);
}

struct TypedValues<T> {
    values: AHashMap<Slot, T>,
}

struct TypedWrites<T> {
    values: Vec<(Slot, T)>,
}

impl<T: Clone + Send + 'static> ErasedValues for TypedValues<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn remove(&mut self, slot: Slot) {
        self.values.remove(&slot);
    }

    fn clear(&mut self) {
        self.values.clear();
    }

    fn commit(&mut self, writes: &mut dyn ErasedWrites) {
        let writes = writes
            .as_any_mut()
            .downcast_mut::<TypedWrites<T>>()
            .expect("CpuWireWrites type table does not match its registration");
        for (slot, value) in writes.values.drain(..) {
            self.values.insert(slot, value);
        }
    }
}

impl<T: Clone + Send + 'static> ErasedWrites for TypedWrites<T> {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn clear(&mut self) {
        self.values.clear();
    }
}

fn make_values<T: Clone + Send + 'static>() -> Box<dyn ErasedValues> {
    Box::new(TypedValues::<T> { values: AHashMap::new() })
}

fn make_writes<T: Clone + Send + 'static>() -> Box<dyn ErasedWrites> {
    Box::new(TypedWrites::<T> { values: Vec::new() })
}

/// A CPU wire payload type submitted to the engine's inventory.
pub struct CpuWireRegistration {
    type_id: TypeId,
    make_values: fn() -> Box<dyn ErasedValues>,
    make_writes: fn() -> Box<dyn ErasedWrites>,
}

impl CpuWireRegistration {
    pub const fn new<T: Clone + Send + 'static>() -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            make_values: make_values::<T>,
            make_writes: make_writes::<T>,
        }
    }
}

inventory::collect!(CpuWireRegistration);

/// Typed CPU values indexed by graph resource slots.
pub struct CpuWireValues {
    tables: AHashMap<TypeId, Box<dyn ErasedValues>>,
}

impl CpuWireValues {
    fn from_registrations<'a, I>(registrations: I) -> Self
    where
        I: IntoIterator<Item = &'a CpuWireRegistration>,
    {
        let mut tables = AHashMap::new();
        for registration in registrations {
            let type_id = registration.type_id;
            if tables.insert(type_id, (registration.make_values)()).is_some() {
                panic!("duplicate CPU wire registration for TypeId {:?}", registration.type_id);
            }
        }
        Self { tables }
    }

    fn table<T: Clone + Send + 'static>(&self) -> &TypedValues<T> {
        let type_id = TypeId::of::<T>();
        self.tables
            .get(&type_id)
            .unwrap_or_else(|| panic!("unregistered CPU wire type {}", type_name::<T>()))
            .as_any()
            .downcast_ref::<TypedValues<T>>()
            .expect("CPU wire value table has the wrong concrete type")
    }

    fn table_mut<T: Clone + Send + 'static>(&mut self) -> &mut TypedValues<T> {
        let type_id = TypeId::of::<T>();
        self.tables
            .get_mut(&type_id)
            .unwrap_or_else(|| panic!("unregistered CPU wire type {}", type_name::<T>()))
            .as_any_mut()
            .downcast_mut::<TypedValues<T>>()
            .expect("CPU wire value table has the wrong concrete type")
    }

    /// Read a cloned value from a registered slot.
    pub fn get<T: Clone + Send + 'static>(&self, slot: Slot) -> Option<T> {
        self.table::<T>().values.get(&slot).cloned()
    }

    /// Set or overwrite a value in a registered slot.
    pub fn set<T: Clone + Send + 'static>(&mut self, slot: Slot, value: T) {
        self.table_mut::<T>().values.insert(slot, value);
    }

    /// Remove a slot from every registered payload table.
    pub fn remove(&mut self, slot: Slot) {
        for table in self.tables.values_mut() {
            table.remove(slot);
        }
    }

    /// Clear all payload values while retaining typed map allocations.
    pub fn clear(&mut self) {
        for table in self.tables.values_mut() {
            table.clear();
        }
    }
}

impl Default for CpuWireValues {
    fn default() -> Self {
        Self::from_registrations(inventory::iter::<CpuWireRegistration>)
    }
}

/// Ordered CPU writes collected during evaluation.
pub struct CpuWireWrites {
    tables: AHashMap<TypeId, Box<dyn ErasedWrites>>,
}

impl CpuWireWrites {
    fn from_registrations<'a, I>(registrations: I) -> Self
    where
        I: IntoIterator<Item = &'a CpuWireRegistration>,
    {
        let mut tables = AHashMap::new();
        for registration in registrations {
            let type_id = registration.type_id;
            if tables.insert(type_id, (registration.make_writes)()).is_some() {
                panic!("duplicate CPU wire registration for TypeId {:?}", registration.type_id);
            }
        }
        Self { tables }
    }

    fn table_mut<T: Clone + Send + 'static>(&mut self) -> &mut TypedWrites<T> {
        let type_id = TypeId::of::<T>();
        self.tables
            .get_mut(&type_id)
            .unwrap_or_else(|| panic!("unregistered CPU wire type {}", type_name::<T>()))
            .as_any_mut()
            .downcast_mut::<TypedWrites<T>>()
            .expect("CPU wire write table has the wrong concrete type")
    }

    /// Append a write, preserving its order until commit.
    pub fn push<T: Clone + Send + 'static>(&mut self, slot: Slot, value: T) {
        self.table_mut::<T>().values.push((slot, value));
    }

    /// Discard pending writes while retaining each typed vector's capacity.
    pub fn clear(&mut self) {
        for table in self.tables.values_mut() {
            table.clear();
        }
    }

    /// Apply writes in insertion order, retaining the write-buffer capacity.
    pub fn commit(&mut self, values: &mut CpuWireValues) {
        for (type_id, writes) in &mut self.tables {
            let destination = values
                .tables
                .get_mut(type_id)
                .unwrap_or_else(|| panic!("CPU wire write has no registered destination"));
            destination.commit(writes.as_mut());
        }
    }
}

impl Default for CpuWireWrites {
    fn default() -> Self {
        Self::from_registrations(inventory::iter::<CpuWireRegistration>)
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    fn registrations() -> [CpuWireRegistration; 2] {
        [CpuWireRegistration::new::<u32>(), CpuWireRegistration::new::<f32>()]
    }

    #[test]
    fn cpu_wire_values_support_multiple_types_and_overwrites() {
        let registrations = registrations();
        let mut values = CpuWireValues::from_registrations(registrations.iter());
        let slot = Slot(4);
        values.set(slot, 1_u32);
        values.set(slot, 2.5_f32);
        values.set(slot, 3_u32);
        assert_eq!(values.get::<u32>(slot), Some(3));
        assert_eq!(values.get::<f32>(slot), Some(2.5));
    }

    #[test]
    fn cpu_wire_commit_preserves_order_and_write_capacity() {
        let registrations = registrations();
        let mut values = CpuWireValues::from_registrations(registrations.iter());
        let mut writes = CpuWireWrites::from_registrations(registrations.iter());
        values.set(Slot(1), 0_u32);
        values.set(Slot(2), 0_u32);
        values.set(Slot(3), 0.0_f32);
        let map_capacity = values.table::<u32>().values.capacity();
        writes.push(Slot(1), 10_u32);
        writes.push(Slot(2), 20_u32);
        writes.push(Slot(1), 30_u32);
        writes.push(Slot(3), 1.5_f32);
        let write_capacity = writes.table_mut::<u32>().values.capacity();

        writes.commit(&mut values);

        assert_eq!(values.get::<u32>(Slot(1)), Some(30));
        assert_eq!(values.get::<u32>(Slot(2)), Some(20));
        assert_eq!(values.get::<f32>(Slot(3)), Some(1.5));
        assert_eq!(values.table::<u32>().values.capacity(), map_capacity);
        assert!(writes.table_mut::<u32>().values.is_empty());
        assert_eq!(writes.table_mut::<u32>().values.capacity(), write_capacity);

        writes.push(Slot(1), 99_u32);
        writes.clear();
        writes.commit(&mut values);
        assert_eq!(values.get::<u32>(Slot(1)), Some(30));
        assert_eq!(values.table::<u32>().values.capacity(), map_capacity);
        assert_eq!(writes.table_mut::<u32>().values.capacity(), write_capacity);
    }

    #[test]
    fn cpu_wire_remove_and_clear_drop_all_types() {
        let registrations = registrations();
        let mut values = CpuWireValues::from_registrations(registrations.iter());
        values.set(Slot(1), 4_u32);
        values.set(Slot(1), 2.0_f32);
        values.set(Slot(2), 5_u32);
        values.remove(Slot(1));
        assert_eq!(values.get::<u32>(Slot(1)), None);
        assert_eq!(values.get::<f32>(Slot(1)), None);
        assert_eq!(values.get::<u32>(Slot(2)), Some(5));
        values.clear();
        assert_eq!(values.get::<u32>(Slot(2)), None);
    }

    #[test]
    fn cpu_wire_unregistered_access_panics_explicitly() {
        let registrations = [CpuWireRegistration::new::<u32>()];
        let mut values = CpuWireValues::from_registrations(registrations.iter());
        let get = catch_unwind(AssertUnwindSafe(|| values.get::<u64>(Slot(0))));
        assert!(get.is_err());
        let set = catch_unwind(AssertUnwindSafe(|| values.set(Slot(0), 1_u64)));
        assert!(set.is_err());

        let mut writes = CpuWireWrites::from_registrations(registrations.iter());
        let push = catch_unwind(AssertUnwindSafe(|| writes.push(Slot(0), 1_u64)));
        assert!(push.is_err());
    }

    #[test]
    fn cpu_wire_duplicate_registration_panics_at_construction() {
        let registrations = [
            CpuWireRegistration::new::<u32>(),
            CpuWireRegistration::new::<u32>(),
        ];
        let values = catch_unwind(AssertUnwindSafe(|| {
            CpuWireValues::from_registrations(registrations.iter())
        }));
        assert!(values.is_err());
        let writes = catch_unwind(AssertUnwindSafe(|| {
            CpuWireWrites::from_registrations(registrations.iter())
        }));
        assert!(writes.is_err());
    }
}
