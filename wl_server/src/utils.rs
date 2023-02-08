/// Why does `Item` look like this? why not using GAT? This comes from a
/// limitation similar to the one outlined in: https://sabrinajewson.org/blog/the-better-alternative-to-lifetime-gats/
///
/// Quickly summarizing, while `AsIterator` is fine on its own, if in another
/// trait we want to express that a method returns something that implements
/// `AsIterator`, and yields `&u32`, we might have to write something like:
///
/// ```ignore
/// trait Foo {
///     type AsIter<'a>: AsIterator<Item<'a> = &'a u32> + 'a;
///     fn bar<'a>(&'a self) -> Self::AsIter<'a>;
/// }
/// pub trait AsIterator {
///     type Item<'a> where Self: 'a;
///     type Iter<'a>: Iterator<Item = Self::Item<'a>> + 'a
///     where
///         Self: 'a;
///     fn as_iter(&self) -> Self::Iter<'_>;
/// }
/// ```
///
/// Then when calling `bar` with `&'a Self`, the returned `AsIter<'a>` has to
/// outlive `'a` because of the `where Self: 'a` requirement on `Item<'a>`,
/// which without knowing the variance of `Self: Foo`, would borrow `self` for
/// its whole lifetime. Besides this also seems to trigger a bug in the borrow
/// checker.
///
/// Another way is to write:
///
/// ```ignore
/// trait Foo {
///     type AsIter<'a>: for<'b> AsIterator<Item<'b> = &'a u32> + 'a;
/// }
/// ```
///
/// Which is somewhat fine, but this requires `AsIter<'a>: 'static` again
/// because of the `where Self: 'a` bound.
///
/// We really would like a `type Item<'a>` that is totally detached from `Self`,
/// so we can put a bound on it without affecting the lifetime of `Self`. One
/// trick that can be used is to emulate lifetime GAT with a HRTB trait object,
/// which is what we are using.
///
/// ```rust
/// trait Foo {
///     type AsIter<'a>: AsIterator<Item = dyn for<'b> AsIteratorItem<'b, Item = &'b u32>> + 'a;
/// }
/// ```
///
/// When you implement `AsIterator` for a type, you should use `dyn for<'a>
/// AsIteratorItem<'a, Item = T>` as the `Item` type.
pub trait AsIteratorItem<'a> {
    type Item;
}

pub trait AsIterator {
    type Item: ?Sized + for<'a> AsIteratorItem<'a>;
    type Iter<'a>: Iterator<Item = <Self::Item as AsIteratorItem<'a>>::Item> + 'a
    where
        Self: 'a;
    fn as_iter(&self) -> Self::Iter<'_>;
}
