//! The provide_any API "borrowed" from the Rust libstd.
//!
//! Remove after the API is stabilized. (see: <https://github.com/rust-lang/rust/issues/96024>)
#![allow(missing_docs, missing_debug_implementations)]

use std::any::TypeId;

///////////////////////////////////////////////////////////////////////////////
// Type tags
///////////////////////////////////////////////////////////////////////////////

mod tags {
    //! Type tags are used to identify a type using a separate value. This
    //! module includes type tags for some very common types.
    //!
    //! Currently type tags are not exposed to the user. But in the future, if
    //! you want to use the Provider API with more complex types (typically
    //! those including lifetime parameters), you will need to write your
    //! own tags.

    use std::marker::PhantomData;

    /// This trait is implemented by specific tag types in order to allow
    /// describing a type which can be requested for a given lifetime `'a`.
    ///
    /// A few example implementations for type-driven tags can be found in this
    /// module, although crates may also implement their own tags for more
    /// complex types with internal lifetimes.
    pub trait Type<'a>: Sized + 'static {
        /// The type of values which may be tagged by this tag for the given
        /// lifetime.
        type Reified: 'a;
    }

    /// Similar to the [`Type`] trait, but represents a type which may be
    /// unsized (i.e., has a `?Sized` bound). E.g., `str`.
    pub trait MaybeSizedType<'a>: Sized + 'static {
        type Reified: 'a + ?Sized;
    }

    impl<'a, T: Type<'a>> MaybeSizedType<'a> for T {
        type Reified = T::Reified;
    }

    /// Type-based tag for types bounded by `'static`, i.e., with no borrowed
    /// elements.
    #[derive(Debug)]
    pub struct Value<T: 'static>(PhantomData<T>);

    impl<T: 'static> Type<'_> for Value<T> {
        type Reified = T;
    }

    /// Type-based tag similar to [`Value`] but which may be unsized (i.e., has
    /// a `?Sized` bound).
    #[derive(Debug)]
    pub struct MaybeSizedValue<T: ?Sized + 'static>(PhantomData<T>);

    impl<T: ?Sized + 'static> MaybeSizedType<'_> for MaybeSizedValue<T> {
        type Reified = T;
    }

    /// Type-based tag for reference types (`&'a T`, where T is represented by
    /// `<I as MaybeSizedType<'a>>::Reified`.
    #[derive(Debug)]
    pub struct Ref<I>(PhantomData<I>);

    impl<'a, I: MaybeSizedType<'a>> Type<'a> for Ref<I> {
        type Reified = &'a I::Reified;
    }

    #[derive(Debug)]
    pub struct RefMut<I>(PhantomData<I>);

    impl<'a, I: MaybeSizedType<'a>> Type<'a> for RefMut<I> {
        type Reified = &'a mut I::Reified;
    }
}
/// An object of arbitrary type. This whole API is stolen from Rust's
/// std::any::Provider. We should remove this when that stabilizes.
pub trait Provider {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>);
    fn provide_mut<'a>(&'a mut self, _demand: &mut Demand<'a>) {}
}

/// Like [`Provider`], but with a "slot" to hint on what's being requested.
/// Normally with `Provider`, the provider needs to provide everything it can
/// provide, leading to `O(n)` time complexity. This trait instead provide a
/// "slot" to hint on what's being requested, allowing providers to provide only
/// what's needed, thus speed things up.
pub trait SlottedProvider {
    fn provide<'a>(&'a self, slot: usize, demand: &mut Demand<'a>);
    fn provide_mut<'a>(&'a mut self, _slot: usize, _demand: &mut Demand<'a>) {}
}

pub struct ProviderArray<const N: usize> {
    pub providers: [Option<Box<dyn Provider>>; N],
}

impl<const N: usize> ProviderArray<N> {
    pub fn set<T: 'static + Provider>(&mut self, slot: usize, provider: T) {
        if slot < N {
            self.providers[slot] = Some(Box::new(provider));
        } else {
            panic!("slot out of range");
        }
    }
}

impl<const N: usize> Default for ProviderArray<N> {
    fn default() -> Self {
        // Needed to work around the required `Copy` bound.
        const NONE: Option<Box<dyn Provider>> = None;
        Self {
            providers: [NONE; N],
        }
    }
}

impl<const N: usize> SlottedProvider for ProviderArray<N> {
    fn provide<'a>(&'a self, slot: usize, demand: &mut Demand<'a>) {
        if slot < N {
            if let Some(provider) = &self.providers[slot] {
                provider.provide(demand);
            }
        }
    }

    fn provide_mut<'a>(&'a mut self, slot: usize, demand: &mut Demand<'a>) {
        if slot < N {
            if let Some(provider) = &mut self.providers[slot] {
                provider.provide_mut(demand);
            }
        }
    }
}

#[repr(transparent)]
pub struct Demand<'a>(dyn Erased<'a> + 'a);

pub struct Receiver<'a, 'b, I: tags::Type<'a>>(&'b mut TaggedOption<'a, I>);

impl<'a, I: tags::Type<'a>> Receiver<'a, '_, I> {
    #[inline]
    pub fn provide(&mut self, value: I::Reified) {
        self.0 .0 = Some(value)
    }
}

impl<'a> Demand<'a> {
    /// Create a new `&mut Demand` from a `&mut dyn Erased` trait object.
    fn new<'b>(erased: &'b mut (dyn Erased<'a> + 'a)) -> &'b mut Demand<'a> {
        // SAFETY: transmuting `&mut (dyn Erased<'a> + 'a)` to `&mut Demand<'a>` is safe
        // since `Demand` is repr(transparent).
        unsafe { &mut *(erased as *mut dyn Erased<'a> as *mut Demand<'a>) }
    }

    /// Provide a value or other type with only static lifetimes.
    ///
    /// # Examples
    ///
    /// Provides a `String` by cloning.
    ///
    /// ```rust
    /// # #![feature(provide_any)]
    /// use std::any::{Demand, Provider};
    /// # struct SomeConcreteType { field: String }
    ///
    /// impl Provider for SomeConcreteType {
    ///     fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
    ///         demand.provide_value::<String, _>(|| self.field.clone());
    ///     }
    /// }
    /// ```
    pub fn provide_value<T, F>(&mut self, fulfil: F) -> &mut Self
    where
        T: 'static,
        F: FnOnce() -> T,
    {
        self.provide_with::<tags::Value<T>, F>(fulfil)
    }

    /// Provide a reference, note that the referee type must be bounded by
    /// `'static`, but may be unsized.
    ///
    /// # Examples
    ///
    /// Provides a reference to a field as a `&str`.
    ///
    /// ```rust
    /// # #![feature(provide_any)]
    /// use std::any::{Demand, Provider};
    /// # struct SomeConcreteType { field: String }
    ///
    /// impl Provider for SomeConcreteType {
    ///     fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
    ///         demand.provide_ref::<str>(&self.field);
    ///     }
    /// }
    /// ```
    pub fn provide_ref<T: ?Sized + 'static>(&mut self, value: &'a T) -> &mut Self {
        self.provide::<tags::Ref<tags::MaybeSizedValue<T>>>(value)
    }

    pub fn provide_mut<T: ?Sized + 'static>(&mut self, value: &'a mut T) -> &mut Self {
        self.provide::<tags::RefMut<tags::MaybeSizedValue<T>>>(value)
    }

    /// Provide a mutable references. But first check if `T` will be accepted.
    ///
    /// This is because `provide_mut` takes a `&'a mut T`, which means once you
    /// called that, you won't be able to provide anything else. Because
    /// it's not possible to have multiple mutable references.
    ///
    /// This method breaks up the process into two steps, first you check if `T`
    /// will be accepted, and you only pass the `&'a mut T` only if it will
    /// be accepted.
    pub fn maybe_provide_mut<T: ?Sized + 'static>(
        &mut self,
    ) -> Option<Receiver<'a, '_, tags::RefMut<tags::MaybeSizedValue<T>>>> {
        self.0
            .downcast_mut::<tags::RefMut<tags::MaybeSizedValue<T>>>()
            .map(Receiver)
    }

    /// Provide a value with the given `Type` tag.
    fn provide<I>(&mut self, value: I::Reified) -> &mut Self
    where
        I: tags::Type<'a>,
    {
        if let Some(res @ TaggedOption(None)) = self.0.downcast_mut::<I>() {
            res.0 = Some(value);
        }
        self
    }

    /// Provide a value with the given `Type` tag, using a closure to prevent
    /// unnecessary work.
    fn provide_with<I, F>(&mut self, fulfil: F) -> &mut Self
    where
        I: tags::Type<'a>,
        F: FnOnce() -> I::Reified,
    {
        if let Some(res @ TaggedOption(None)) = self.0.downcast_mut::<I>() {
            res.0 = Some(fulfil());
        }
        self
    }
}

impl std::fmt::Debug for Demand<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Demand").finish_non_exhaustive()
    }
}

/// Represents a type-erased but identifiable object.
///
/// This trait is exclusively implemented by the `TaggedOption` type.
///
/// # Safety
///
/// If this trait is not implemented correctly, downcast_mut will cast to the
/// wrong type and cause undefined behavior.
unsafe trait Erased<'a>: 'a {
    /// The `TypeId` of the erased type.
    fn tag_id(&self) -> TypeId;
}

unsafe impl<'a, I: tags::Type<'a>> Erased<'a> for TaggedOption<'a, I> {
    fn tag_id(&self) -> TypeId {
        TypeId::of::<I>()
    }
}

impl<'a> dyn Erased<'a> + 'a {
    /// Returns some reference to the dynamic value if it is tagged with `I`,
    /// or `None` otherwise.
    #[inline]
    fn downcast_mut<I>(&mut self) -> Option<&mut TaggedOption<'a, I>>
    where
        I: tags::Type<'a>,
    {
        if self.tag_id() == TypeId::of::<I>() {
            // SAFETY: Just checked whether we're pointing to an I.
            Some(unsafe { &mut *(self as *mut Self).cast::<TaggedOption<'a, I>>() })
        } else {
            None
        }
    }
}

#[repr(transparent)]
struct TaggedOption<'a, I: tags::Type<'a>>(Option<I::Reified>);

impl<'a, I: tags::Type<'a>> TaggedOption<'a, I> {
    fn as_demand(&mut self) -> &mut Demand<'a> {
        Demand::new(self as &mut (dyn Erased<'a> + 'a))
    }
}

pub fn request_ref<'a, T, P>(provider: &'a P) -> Option<&'a T>
where
    T: 'static + ?Sized,
    P: Provider + ?Sized,
{
    request_by_type_tag::<'a, tags::Ref<tags::MaybeSizedValue<T>>, P>(provider)
}

pub fn request_ref_by_slot<'a, T, P>(provider: &'a P, slot: usize) -> Option<&'a T>
where
    T: 'static + ?Sized,
    P: SlottedProvider + ?Sized,
{
    let mut tagged = TaggedOption::<'a, tags::Ref<tags::MaybeSizedValue<T>>>(None);
    provider.provide(slot, tagged.as_demand());
    tagged.0
}

pub fn request_mut<'a, T, P>(provider: &'a mut P) -> Option<&'a mut T>
where
    T: 'static + ?Sized,
    P: Provider + ?Sized,
{
    let mut tagged = TaggedOption::<'a, tags::RefMut<tags::MaybeSizedValue<T>>>(None);
    provider.provide_mut(tagged.as_demand());
    tagged.0
}

/// Request a specific value by tag from the `Provider`.
fn request_by_type_tag<'a, I, P>(provider: &'a P) -> Option<I::Reified>
where
    I: tags::Type<'a>,
    P: Provider + ?Sized,
{
    let mut tagged = TaggedOption::<'a, I>(None);
    provider.provide(tagged.as_demand());
    tagged.0
}

impl Provider for () {
    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        demand.provide_mut(self);
    }
}
