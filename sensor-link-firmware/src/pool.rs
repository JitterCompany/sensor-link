use core::{marker::PhantomData, ops::Deref};

/// Pool: manages a fixed amount of objects of the same type.
///
/// Up to at least [Pool::MIN_CAPACITY] items can be allocated from the pool.
/// Once an item is dropped, it can be allocated again.
pub trait Pool: Sized {
    type Data: 'static;
    type Arc: Arc<Data = Self::Data>;
    type ArcBlock: ArcBlock<Self::Data>;
    const MIN_CAPACITY: usize;

    /// Access the pool allocator
    ///
    /// This ensures the internal pool is initialized.
    fn allocator(
        &self,
    ) -> impl PoolAlloc<
        Data = <Self as Pool>::Data,
        Arc = <Self as Pool>::Arc,
        ArcBlock = <Self as Pool>::ArcBlock,
    >;
}

/// Pool: manages a fixed amount of objects of the same type.
///
/// Up to [Self::capacity()] items can be allocated from a pool.
/// Once an item is dropped, it can be allocated again.
pub trait PoolAlloc: Sized {
    type Data: 'static;
    type Arc: Arc<Data = Self::Data>;
    type ArcBlock: ArcBlock<Self::Data>;

    /// Get the total pool capacity. This is the maximum amount of [Arc]s that can exist
    /// at the same time and is guaranteed to be at least [Pool::MIN_CAPACITY]
    fn capacity(&self) -> usize;

    /// Add capacity to the pool. This increases the pool capacity with `blocks.len()`
    fn add(&mut self, blocks: &'static mut [Self::ArcBlock]);

    /// Try to allocate a new item from the pool.
    ///
    /// If successfull, the data is moved to the pool and a platform-specific
    /// 'smart pointer' is returned which implements [Arc].
    fn alloc(
        &self,
        value: <Self as PoolAlloc>::Data,
    ) -> Result<<Self as PoolAlloc>::Arc, <Self as PoolAlloc>::Data>;
}

/// Immutable refcounted pooled item.
///
/// All clones share the same data, data is dropped after all clones are dropped.
pub trait Arc: Ref<Self::Data> + Clone + Send {
    type Data: 'static;
    type Pool: Pool<Arc = Self>;
}

/// Immutable pooled item
pub trait Ref<D>: AsRef<D> + core::ops::Deref<Target = D> {}

/// Block of memory that can be managed by [Pool] as backing storage for an Arc
pub trait ArcBlock<T> {}

/// Platform-independent [Arc]
pub struct GenericArc<P: Pool, PA> {
    _pool: PhantomData<P>,
    pub _inner: PA,
}
impl<P: Pool, PA> GenericArc<P, PA> {
    pub fn new(inner: PA) -> Self {
        Self {
            _pool: PhantomData,
            _inner: inner,
        }
    }
}

// Transparent debug impl
impl<P: Pool, PA> core::fmt::Debug for GenericArc<P, PA>
where
    PA: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self._inner.fmt(f)
    }
}

impl<P: Pool, D, PA> Ref<D> for GenericArc<P, PA> where PA: Ref<D> {}

impl<P: Pool, D, PA> AsRef<D> for GenericArc<P, PA>
where
    PA: AsRef<D>,
{
    #[inline]
    fn as_ref(&self) -> &D {
        self._inner.as_ref()
    }
}

impl<P: Pool, D, PA> Deref for GenericArc<P, PA>
where
    PA: Deref<Target = D>,
{
    type Target = D;

    fn deref(&self) -> &Self::Target {
        self._inner.deref()
    }
}

impl<P: Pool, PA> Clone for GenericArc<P, PA>
where
    PA: Clone,
{
    fn clone(&self) -> Self {
        Self {
            _pool: PhantomData,
            _inner: self._inner.clone(),
        }
    }
}

/// Helper struct to create a mapped allocator
///
/// This can be used to create a new allocator that maps the output type of the allocator
/// to a different type.
pub struct Mapper<'a, A: PoolAlloc, O, F: Fn(<A as PoolAlloc>::Arc) -> O> {
    allocator: &'a A,
    transformer: F,
}

impl<'a, A: PoolAlloc, O, F: Fn(<A as PoolAlloc>::Arc) -> O> Mapper<'a, A, O, F> {
    /// Wrap an allocator with a transform function.
    /// Creates a new allocator that maps the output type of the allocator to a different type.
    pub fn new(allocator: &'a A, transformer: F) -> Self {
        Self {
            allocator,
            transformer,
        }
    }
}

impl<'a, A: PoolAlloc, O, F: Fn(<A as PoolAlloc>::Arc) -> O> MappedAllocator
    for Mapper<'a, A, O, F>
{
    type Input = <A as PoolAlloc>::Data;
    type Output = O;

    fn alloc(&self, value: Self::Input) -> Result<Self::Output, Self::Input> {
        self.allocator.alloc(value).map(&self.transformer)
    }
}

pub trait MappedAllocator {
    type Input;
    type Output;

    /// Try to allocate a new item of type [Input](MappedAllocator::Input)
    ///
    /// If successfull, the data is moved to the pool and a platform-specific
    /// 'smart pointer' is mapped to the output type [Output](MappedAllocator::Output).
    fn alloc(&self, value: Self::Input) -> Result<Self::Output, Self::Input>;
}

// no_std: use heapless:pool. Only available on arm_llsc / x86
#[cfg(all(any(arm_llsc, target_arch = "x86"), not(feature = "use-std")))]
pub mod platform {

    use super::{Arc, ArcBlock, Pool, Ref};
    pub use crate::heapless::pool::arc::{
        Arc as HeaplessArc, ArcBlock as HeaplessArcBlock, ArcPool as HeaplessPool,
    };

    pub type PlatformArc<P, HP> = super::GenericArc<P, HeaplessArc<HP>>;
    pub type PlatformArcBlock<D> = HeaplessArcBlock<D>;

    // Heapless:Arc already implements Clone,Asref,Deref
    impl<D: 'static, P: HeaplessPool<Data = D>> Ref<D> for HeaplessArc<P> {}
    impl<D> ArcBlock<D> for HeaplessArcBlock<D> {}

    impl<
            D: 'static + Send + Sync,
            HP: HeaplessPool<Data = D>,
            P: Pool<Arc = Self, Data = D> + Send + Sync,
        > Arc for PlatformArc<P, HP>
    {
        type Data = D;
        type Pool = P;
    }

    #[macro_export]
    macro_rules! define_pool {
        ($name:ident, $data_type:ty, $len:expr) => {

            $crate::paste::paste! {

                // Hide inner implementation details in private module
                mod [< $name:snake _inner >] {
                    $crate::heapless::arc_pool!($name: super::$data_type);
                    pub static CAPACITY: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new($len);
                }

                pub struct $name;
                impl $crate::pool::Pool for $name {
                    type Arc = $crate::pool::platform::PlatformArc<$name, [< $name:snake _inner >] :: $name>;
                    type Data = $data_type;
                    type ArcBlock = $crate::heapless::pool::arc::ArcBlock<$data_type>;

                    const MIN_CAPACITY : usize = $len;

                    fn allocator(
                        &self,
                    ) -> [< $name:camel Alloc >] {

                        // 1. make sure the pool is initialized
                        [<try_init_ $name:snake >]();

                        // 2. return a PoolAlloc impl
                        [< $name:camel Alloc >] {
                            _private: core::marker::PhantomData,
                            capacity: [< $name:snake _inner >] :: CAPACITY.load(core::sync::atomic::Ordering::Relaxed)
                        }
                    }
                }

                pub struct [< $name:camel Alloc >] {
                    _private: core::marker::PhantomData<()>,
                    capacity: usize,
                }
                impl $crate::pool::PoolAlloc for [< $name:camel Alloc >] {
                    type Arc = $crate::pool::platform::PlatformArc<$name, [< $name:snake _inner >] :: $name>;
                    type Data = $data_type;
                    type ArcBlock =  $crate::heapless::pool::arc::ArcBlock<$data_type>;

                    /// Capacity of the pool
                    ///
                    /// The actual capacity could be larger in case more capacity was added
                    /// after this [PoolAlloc] instance was created. In that edge-case,
                    /// create a new instance or refresh by calling `self.add(&[])`
                    fn capacity(&self) -> usize {
                        self.capacity
                    }

                    /// Add a slice of objects to the pool
                    ///
                    /// This increases the pool capacity by `blocks.len()`.
                    fn add(&mut self, blocks: &'static mut [Self::ArcBlock]) {
                        let extra_capacity = blocks.len();
                        for block in blocks {
                            [< $name:snake _inner >] :: $name . manage(block);
                        }

                        // new capacity is increased by at least extra_capacity
                        // (other instances could have also added capacity in the meantime)
                        let prev_cap = [< $name:snake _inner >] :: CAPACITY.fetch_add(extra_capacity, core::sync::atomic::Ordering::Relaxed);
                        self.capacity = prev_cap + extra_capacity;
                    }

                    #[inline]
                    fn alloc(&self,
                        value: $data_type,
                    ) -> Result<<Self as $crate::pool::PoolAlloc>::Arc, $data_type> {
                        let res = [< $name:snake _inner >] :: $name . alloc(value)?;
                        Ok($crate::pool::platform::PlatformArc::new(res))
                    }
                }

                /// Initialize the pool (if not already)
                pub fn [<try_init_ $name:snake >] () {
                    use $crate::heapless::pool::arc::ArcBlock as HeaplessArcBlock;

                    static BORROWED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

                    // Try to initialize
                    if BORROWED.compare_exchange(false, true,
                        core::sync::atomic::Ordering::Release,
                        core::sync::atomic::Ordering::Acquire).is_ok() {

                        let blocks: &'static mut [HeaplessArcBlock<$data_type>] = {
                                const BLOCK: HeaplessArcBlock<$data_type> = HeaplessArcBlock::new();
                                static mut BLOCKS : [HeaplessArcBlock<$data_type>; $len] = [BLOCK; $len];
                                unsafe { &mut BLOCKS }
                            };


                        for block in blocks {
                            [< $name:snake _inner >] :: $name . manage(block);
                        }
                    }

                }
            }
        };
    }
}

// use-std or test on platform without heapless:pool support: fallback to std implementation
#[cfg(any(test, feature = "use-std"))]
pub mod platform {

    use super::{Arc, Pool, Ref};
    use std::sync::Arc as StdArc;

    pub type PlatformArc<P> = super::GenericArc<P, StdArc<<P as Pool>::Data>>;

    impl<D> Ref<D> for StdArc<D> {}

    #[derive(Default)]
    pub struct PlatformArcBlock<D>(core::marker::PhantomData<D>);
    impl<D> super::ArcBlock<D> for PlatformArcBlock<D> {}

    impl<D: 'static + Send + Sync, P: Pool<Arc = Self, Data = D> + Send> Arc
        for super::GenericArc<P, StdArc<D>>
    {
        type Data = D;
        type Pool = P;
    }

    #[macro_export]
    macro_rules! define_pool {
        ($name:ident, $data_type:ty, $len:expr) => {
            $crate::paste::paste! {

                pub struct $name;
                impl $crate::pool::Pool for $name {
                    type Arc = $crate::pool::platform::PlatformArc<$name>;
                    type Data = $data_type;
                    type ArcBlock = $crate::pool::platform::PlatformArcBlock<$data_type>;
                    const MIN_CAPACITY : usize = $len;

                    fn allocator(
                        &self,
                    ) -> impl $crate::pool::PoolAlloc<Data = <Self as $crate::pool::Pool>::Data, Arc = <Self as $crate::pool::Pool>::Arc, ArcBlock = <Self as $crate::pool::Pool>::ArcBlock> {

                        [< $name:camel Alloc >] {
                            _private: core::marker::PhantomData,
                            capacity: Self::MIN_CAPACITY
                        }
                    }
                }

                pub struct [< $name:camel Alloc >] {
                    _private: core::marker::PhantomData<()>,
                    capacity: usize,
                }
                impl $crate::pool::PoolAlloc for [< $name:camel Alloc >] {
                    type Arc = $crate::pool::platform::PlatformArc<$name>;
                    type Data = $data_type;
                    type ArcBlock = $crate::pool::platform::PlatformArcBlock<$data_type>;

                    /// Capacity of the pool
                    ///
                    /// The actual capacity could be larger in case more capacity was added
                    /// after this [PoolAlloc] instance was created. In that edge-case,
                    /// create a new instance or refresh by calling `self.add(&[])`
                    fn capacity(&self) -> usize {
                        self.capacity
                    }

                    /// Add a slice of objects to the pool
                    ///
                    /// This increases the pool capacity by `blocks.len()`.
                    fn add(&mut self, blocks: &'static mut [Self::ArcBlock]) {
                        let extra_capacity = blocks.len();

                        // TODO actually add capacity to the pool.
                        // for now alloc() always succeeds independent of the capacity
                        self.capacity+= extra_capacity;
                    }

                    #[inline]
                    fn alloc(&self,
                        value: $data_type,
                    ) -> Result<<Self as $crate::pool::PoolAlloc>::Arc, $data_type> {
                        // TODO only allow self.capacity() Arcs to exist at the same time!
                        let res = std::sync::Arc::new(value);
                        Ok($crate::pool::platform::PlatformArc::new(res))
                    }
                }
            }
        };
    }
}

// No pool backend exists for this target/feature combination: neither `use-std`
// nor `test` (which select the std backend above) and not a target with heapless
// arc-pool support (`arm_llsc`/`x86`). This covers host no_std builds such as a
// no-default-features dependant on a non-x86 host. Define `define_pool!` anyway,
// so crates that merely re-export it still compile; an actual invocation fails
// with a clear message instead of "cannot find macro `define_pool`".
//
// The cfg is the exact complement of the two backend modules above, so exactly
// one `define_pool!` is `#[macro_export]`ed in any configuration.
#[cfg(all(
    not(any(test, feature = "use-std")),
    not(any(arm_llsc, target_arch = "x86"))
))]
#[macro_export]
macro_rules! define_pool {
    ($($tt:tt)*) => {
        compile_error!(
            "define_pool! has no pool backend in this configuration: enable the \
             `use-std` feature, or build for a target with heapless arc-pool \
             support (arm_llsc / x86)"
        );
    };
}
