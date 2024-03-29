//! Types for geometry calculations.
//!
//! This is largely adapted from the `smithay` crate.

use std::{
    fmt,
    ops::{Add, AddAssign, Div, Mul, Sub, SubAssign},
};

use num_traits::{AsPrimitive, One, SaturatingAdd, SaturatingMul, SaturatingSub, Zero};
use ordered_float::NotNan;

/// Tag types for makring the coordinate system of a value.
pub mod coords {
    /// Screen coordinates. The compositor can have a virutal "screen",
    /// essentially a unified canvas where all the windows are drawn. This
    /// is that coordinate system.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Screen;

    /// Scale invariant screen coordinates. Used to determine intersection
    /// between surfaces and outputs. The reason for this is, the actual
    /// pixel size of a surface can change depends on how it's scaled, which
    /// depends on which output an surface is on. If intersection between
    /// surface and output is calculated using the pixel coordinates, there wll
    /// be a circular dependency, which could create a feedback loop.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ScreenNormalized;

    /// Coordinates local to an output. An output is usually a transformed part
    /// of the screen.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Output;

    /// Surface local coordinates, for positioning subsurfaces (and maybe
    /// popups). This is x to the left, y down
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Surface;

    /// Size/location in physical units like meters, inches, etc.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Physical;

    /// Buffer local coordinates.
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Buffer;

    /// A tag trait for coordinate spaces, sealed so that only the types defined
    /// in this module can implement it.
    pub trait CoordinateSpace: Clone + Copy + Default + PartialEq + Eq + std::fmt::Debug {}

    /// Mapping from one coordinate space to another.
    pub trait Map: Sized {
        /// The output type with the new coordinate space.
        type Output<Kind>
        where
            Kind: CoordinateSpace;

        /// Map from one coordinate space to another by applying a function.
        fn map<Kind: CoordinateSpace>(
            self,
            f: impl FnOnce(Self::Output<Kind>) -> Self::Output<Kind>,
        ) -> Self::Output<Kind>;
    }

    impl CoordinateSpace for Screen {}
    impl CoordinateSpace for ScreenNormalized {}
    impl CoordinateSpace for Output {}
    impl CoordinateSpace for Surface {}
    impl CoordinateSpace for Physical {}
    impl CoordinateSpace for Buffer {}
}

use coords::*;

/// A trait about the sign of a number.
///
/// We need this because `num_traits` does not define `abs` for
/// unsigned numbers.
pub trait Sign {
    /// Return true if the number is negative
    fn is_negative(&self) -> bool;
    /// Return the absolute value of the number
    fn abs(&self) -> Self;
}

macro_rules! impl_sign {
    (signed: $($tys:ty),*; unsigned: $($utys:ty),* ) => {
        $(
            impl Sign for $tys {
                #[inline]
                fn is_negative(&self) -> bool {
                    *self < Self::zero()
                }
                #[inline]
                fn abs(&self) -> Self {
                    num_traits::Signed::abs(self)
                }
            }
        )*
        $(
            impl Sign for $utys {
                #[inline]
                fn is_negative(&self) -> bool {
                    false
                }
                #[inline]
                fn abs(&self) -> Self {
                    *self
                }
            }
        )*
    };
}

/// Trait for approximating a number to the nearest integer
pub trait Approx {
    /// Return the largest integer less than or equal to the number
    fn floor(self) -> Self;
    /// Return the smallest integer greater than or equal to the number
    fn ceil(self) -> Self;
    /// Return the nearest integer to the number
    fn round(self) -> Self;
}

macro_rules! impl_approx {
    (int: $($itys:ty),*; float: $($ftys:ty),*; notnan: $($nntys:ty),*) => {
        $(
            impl Approx for $itys {
                #[inline]
                fn floor(self) -> Self {
                    self
                }

                #[inline]
                fn ceil(self) -> Self {
                    self
                }

                #[inline]
                fn round(self) -> Self {
                    self
                }
            }
        )*
        $(
            impl Approx for $ftys {
                #[inline]
                fn floor(self) -> Self {
                    self.floor()
                }

                #[inline]
                fn ceil(self) -> Self {
                    self.ceil()
                }

                #[inline]
                fn round(self) -> Self {
                    self.round()
                }
            }
        )*
        $(
            impl Approx for NotNan<$nntys> {
                #[inline]
                fn floor(self) -> Self {
                    // Safety: `NotNan` guarantees that the value is not NaN,
                    // and `floor` only returns NaN if the input is NaN.
                    unsafe {
                        NotNan::new_unchecked(self.into_inner().floor())
                    }
                }

                #[inline]
                fn ceil(self) -> Self {
                    // Safety: `NotNan` guarantees that the value is not NaN,
                    // and `ceil` only returns NaN if the input is NaN.
                    unsafe {
                        NotNan::new_unchecked(self.into_inner().ceil())
                    }
                }

                #[inline]
                fn round(self) -> Self {
                    // Safety: `NotNan` guarantees that the value is not NaN,
                    // and `round` only returns NaN if the input is NaN.
                    unsafe {
                        NotNan::new_unchecked(self.into_inner().round())
                    }
                }
            }
        )*
    };
}

impl_sign!(signed: i8, i16, i32, i64, i128, isize, f32, f64, NotNan<f32>, NotNan<f64>;
           unsigned: u8, u16, u32, u64, u128, usize);

impl_approx!(int: i8, i16, i32, i64, i128, u8, u16, u32, u64, u128, usize, isize;
             float: f32, f64; notnan: f32, f64);

/*
 * Scale
 */

/// A two-dimensional scale
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scale<N: Copy> {
    /// The scale on the x axis
    pub x: N,
    /// The scale on the y axis
    pub y: N,
}

impl<N: Copy> Scale<N> {
    /// Convert the underlying numerical type to f64 for floating point
    /// manipulations
    #[inline]
    pub fn to<M: Copy + 'static>(self) -> Scale<M>
    where
        N: AsPrimitive<M>,
    {
        Scale {
            x: self.x.as_(),
            y: self.y.as_(),
        }
    }

    /// Apply a function to the scale. This function is passed x and y scales
    /// independently.
    #[inline]
    pub fn map<M: Copy + 'static>(self, f: impl Fn(N) -> M) -> Scale<M> {
        Scale {
            x: f(self.x),
            y: f(self.y),
        }
    }
}

impl<N: Copy> Scale<N> {
    /// Create a new scale
    #[inline]
    pub fn new(x: N, y: N) -> Self {
        Scale { x, y }
    }

    /// Create a scale that scales uniformly in both directions
    #[inline]
    pub fn uniform(scale: N) -> Self {
        Scale { x: scale, y: scale }
    }
}

impl<N: Copy + One + Div<Output = N> + 'static> Scale<N> {
    /// Caculate the inverse of a scale
    #[inline]
    pub fn inv(self) -> Self {
        Scale {
            x: N::one() / self.x,
            y: N::one() / self.y,
        }
    }
}

impl<N, N2> Mul<Scale<N2>> for Scale<N>
where
    N: Mul<N2> + Copy,
    N2: Copy,
    <N as Mul<N2>>::Output: Copy,
{
    type Output = Scale<<N as Mul<N2>>::Output>;

    #[inline]
    fn mul(self, rhs: Scale<N2>) -> Self::Output {
        Scale {
            x: self.x * rhs.x,
            y: self.y * rhs.y,
        }
    }
}

impl<N> SaturatingMul for Scale<N>
where
    N: SaturatingMul<Output = N> + Copy,
{
    #[inline]
    fn saturating_mul(&self, rhs: &Scale<N>) -> Self::Output {
        Scale {
            x: self.x.saturating_mul(&rhs.x),
            y: self.y.saturating_mul(&rhs.y),
        }
    }
}
impl<N, N2> Div<Scale<N2>> for Scale<N>
where
    N: Div<N2> + Copy,
    N2: Copy,
    <N as Div<N2>>::Output: Copy,
{
    type Output = Scale<<N as Div<N2>>::Output>;

    #[inline]
    fn div(self, rhs: Scale<N2>) -> Self::Output {
        Scale {
            x: self.x / rhs.x,
            y: self.y / rhs.y,
        }
    }
}

/*
 * Point
 */

/// A point as defined by its x and y coordinates
///
/// Operations on points are saturating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Point<N, Kind: CoordinateSpace> {
    /// horizontal coordinate
    pub x: N,
    /// vertical coordinate
    pub y: N,
    _kind: Kind,
}

impl<N, Kind: CoordinateSpace> coords::Map for Point<N, Kind> {
    type Output<Kind2> = Point<N, Kind2> where Kind2: CoordinateSpace;

    fn map<Kind2: CoordinateSpace>(
        self,
        f: impl FnOnce(Point<N, Kind2>) -> Point<N, Kind2>,
    ) -> Point<N, Kind2> {
        let mapped = Point::new(self.x, self.y);
        f(mapped)
    }
}

impl<N, Kind: CoordinateSpace> Point<N, Kind> {
    /// Create a new point
    #[inline]
    pub fn new(x: N, y: N) -> Self {
        Point {
            x,
            y,
            _kind: Kind::default(),
        }
    }
}

impl<N: Sign + Copy + fmt::Debug, Kind: CoordinateSpace> Point<N, Kind> {
    /// Convert this [`Point`] to a [`Extent`] with the same coordinates
    ///
    /// Checks that the coordinates are positive with a `debug_assert!()`.
    #[inline]
    pub fn to_size(self) -> Extent<N, Kind> {
        debug_assert!(
            !self.x.is_negative() && !self.y.is_negative(),
            "Attempting to create a `Extent` of negative size: {:?}",
            (self.x, self.y)
        );
        Extent::new(self.x, self.y)
    }

    /// Convert this [`Point`] to a [`Extent`] with the same coordinates
    ///
    /// Ensures that the coordinates are positive by taking their absolute value
    #[inline]
    pub fn to_size_abs(self) -> Extent<N, Kind> {
        Extent::new(self.x.abs(), self.y.abs())
    }
}
impl<N: Copy + SaturatingMul, Kind: CoordinateSpace> Point<N, Kind> {
    /// Multiply this point by a scale, if the result will overflow,
    /// saturate to the maximum value.
    #[inline]
    pub fn saturating_mul(self, scale: Scale<N>) -> Point<N, Kind> {
        Point::new(
            self.x.saturating_mul(&scale.x),
            self.y.saturating_mul(&scale.y),
        )
    }
}

impl<N: Copy + Mul<Output = N>, Kind: CoordinateSpace> Mul<Scale<N>> for Point<N, Kind> {
    type Output = Self;

    #[inline]
    fn mul(self, scale: Scale<N>) -> Self::Output {
        Point::new(self.x * scale.x, self.y * scale.y)
    }
}

impl<N: Copy + Div<Output = N>, Kind: CoordinateSpace> Div<Scale<N>> for Point<N, Kind> {
    type Output = Self;

    #[inline]
    fn div(self, scale: Scale<N>) -> Self::Output {
        Point::new(self.x / scale.x, self.y / scale.y)
    }
}

impl<N: Ord + Copy, Kind: CoordinateSpace> Point<N, Kind> {
    /// Clamp this [`Point`] to within a [`Rectangle`] with in the same
    /// coordinate space.
    ///
    /// The [`Point`] returned is guaranteed to be within the [`Rectangle`]
    #[inline]
    pub fn clamp(self, rect: Rectangle<N, Kind>) -> Point<N, Kind> {
        Point::new(
            self.x.clamp(rect.loc.x, rect.size.w),
            self.y.clamp(rect.loc.y, rect.size.h),
        )
    }
}

impl<N: Approx, Kind: CoordinateSpace> Point<N, Kind> {
    /// Round the coordinates to the nearest integer
    #[inline]
    pub fn round(self) -> Self {
        Point::new(self.x.round(), self.y.round())
    }

    /// Truncate the coordinates to the largest integer less than or equal to
    /// the coordinate.
    #[inline]
    pub fn floor(self) -> Self {
        Point::new(self.x.floor(), self.y.floor())
    }

    /// Round up the coordinates to the smallest integer greater than or equal
    /// to the coordinate.
    #[inline]
    pub fn ceil(self) -> Self {
        Point::new(self.x.ceil(), self.y.ceil())
    }
}

impl<N, Kind: CoordinateSpace> Point<N, Kind> {
    /// Convert the number type of a point.
    #[inline]
    pub fn to<M: Copy + 'static>(self) -> Point<M, Kind>
    where
        N: AsPrimitive<M>,
    {
        Point::new(self.x.as_(), self.y.as_())
    }
}

impl<N: Add<Output = N> + Copy, Kind: CoordinateSpace> Add for Point<N, Kind> {
    type Output = Point<N, Kind>;

    #[inline]
    fn add(self, other: Point<N, Kind>) -> Point<N, Kind> {
        Point::new(self.x + other.x, self.y + other.y)
    }
}

impl<N: SaturatingAdd + Copy, Kind: CoordinateSpace> SaturatingAdd for Point<N, Kind> {
    #[inline]
    fn saturating_add(&self, other: &Self) -> Self {
        Point::new(
            self.x.saturating_add(&other.x),
            self.y.saturating_add(&other.y),
        )
    }
}

impl<N: Add<Output = N> + Copy, Kind: CoordinateSpace> AddAssign for Point<N, Kind> {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl<N: Sub<Output = N> + Copy, Kind: CoordinateSpace> Sub for Point<N, Kind> {
    type Output = Point<N, Kind>;

    #[inline]
    fn sub(self, other: Point<N, Kind>) -> Point<N, Kind> {
        Point::new(self.x - other.x, self.y - other.y)
    }
}

impl<N: Sub<Output = N> + Copy, Kind: CoordinateSpace> SubAssign for Point<N, Kind> {
    #[inline]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl<N: Zero + Copy, Kind: CoordinateSpace> Default for Point<N, Kind> {
    fn default() -> Self {
        Point::new(N::zero(), N::zero())
    }
}

/*
 * Extent
 */

/// A size as defined by its width and height
///
/// Constructors of this type ensure that the values are always positive via
/// `debug_assert!()`, however manually changing the values of the fields
/// can break this invariant.
///
/// Operations on sizes are saturating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Extent<N, Kind: CoordinateSpace> {
    /// width
    pub w: N,
    /// height
    pub h: N,
    _kind: Kind,
}

impl<N: Sign, Kind: CoordinateSpace> coords::Map for Extent<N, Kind> {
    type Output<Kind2> = Extent<N, Kind2> where Kind2: CoordinateSpace;

    #[inline]
    /// Convert this physical point to logical coordinate space by scaling down
    /// according to given scale factor
    fn map<Kind2: CoordinateSpace>(
        self,
        f: impl FnOnce(Extent<N, Kind2>) -> Extent<N, Kind2>,
    ) -> Extent<N, Kind2> {
        let mapped = Extent::new(self.w, self.h);
        f(mapped)
    }
}

impl<N: Ord + Sign, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Restrict this [`Extent`] to be within another [`Extent`] in the same
    /// coordinate system
    pub fn clamp(self, min: Extent<N, Kind>, max: Extent<N, Kind>) -> Extent<N, Kind> {
        Extent::new(self.w.clamp(min.w, max.w), self.h.clamp(min.h, max.h))
    }
}

impl<N, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Convert the underlying numerical type to f64 for floating point
    /// manipulations
    #[inline]
    pub fn to<M: Copy + Sign + 'static>(self) -> Extent<M, Kind>
    where
        N: AsPrimitive<M>,
    {
        Extent::new(self.w.as_(), self.h.as_())
    }
}
impl<N: Sign, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Create a new extent from width and height
    #[inline]
    pub fn new(w: N, h: N) -> Self {
        debug_assert!(!w.is_negative());
        debug_assert!(!h.is_negative());
        Extent {
            w,
            h,
            _kind: Default::default(),
        }
    }
}

impl<N: Sign + Copy + Ord, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Returns whether a point is contained in `(0, 0)..=(w, h)`
    #[inline]
    pub fn contains(&self, p: Point<N, Kind>) -> bool {
        !p.x.is_negative() && !p.y.is_negative() && p.x <= self.w && p.y <= self.h
    }
}

impl<N: SaturatingMul + Sign + Copy, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Upscale this [`Extent`] by a specified [`Scale`]
    #[inline]
    pub fn saturating_mul(self, scale: Scale<N>) -> Extent<N, Kind> {
        Extent::new(
            self.w.saturating_mul(&scale.x),
            self.h.saturating_mul(&scale.y),
        )
    }
}
impl<N: Mul<Output = N> + Sign + Copy, Kind: CoordinateSpace> Mul<Scale<N>> for Extent<N, Kind> {
    type Output = Extent<N, Kind>;

    #[inline]
    fn mul(self, scale: Scale<N>) -> Extent<N, Kind> {
        Extent::new(self.w * scale.x, self.h * scale.y)
    }
}
impl<N: Div<Output = N> + Sign + Copy, Kind: CoordinateSpace> Div<Scale<N>> for Extent<N, Kind> {
    type Output = Self;

    /// Downscale this [`Extent`] by a specified [`Scale`]
    #[inline]
    fn div(self, scale: Scale<N>) -> Extent<N, Kind> {
        Extent::new(self.w / scale.x, self.h / scale.y)
    }
}
impl<N: Zero + Eq, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Check if this [`Extent`] is empty
    ///
    /// Returns true if either the width or the height is zero
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.w == N::zero() || self.h == N::zero()
    }
}

impl<N: Approx + Sign, Kind: CoordinateSpace> Extent<N, Kind> {
    /// Convert to i32 for integer-space manipulations by rounding float values
    #[inline]
    pub fn round(self) -> Extent<N, Kind> {
        Extent::new(self.w.round(), self.h.round())
    }

    /// Convert to i32 for integer-space manipulations by flooring float values
    #[inline]
    pub fn floor(self) -> Extent<N, Kind> {
        Extent::new(self.w.floor(), self.h.floor())
    }

    /// Convert to i32 for integer-space manipulations by ceiling float values
    #[inline]
    pub fn ceil(self) -> Extent<N, Kind> {
        Extent::new(self.w.ceil(), self.h.ceil())
    }
}

impl<N: Add<Output = N> + Sign + Copy, Kind: CoordinateSpace> Add for Extent<N, Kind> {
    type Output = Self;

    #[inline]
    fn add(self, other: Extent<N, Kind>) -> Extent<N, Kind> {
        Extent::new(self.w + other.w, self.h + other.h)
    }
}

impl<N: Add<Output = N> + Sign + Copy, Kind: CoordinateSpace> AddAssign for Extent<N, Kind> {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl<N: SaturatingAdd + Sign + Copy, Kind: CoordinateSpace> SaturatingAdd for Extent<N, Kind> {
    #[inline]
    fn saturating_add(&self, other: &Self) -> Self {
        Extent::new(
            self.w.saturating_add(&other.w),
            self.h.saturating_add(&other.h),
        )
    }
}

impl<N: Sub<Output = N> + Sign + Copy, Kind: CoordinateSpace> Sub for Extent<N, Kind> {
    type Output = Self;

    #[inline]
    fn sub(self, other: Extent<N, Kind>) -> Extent<N, Kind> {
        Extent::new(self.w - other.w, self.h - other.h)
    }
}

impl<N: SaturatingSub + Ord + Sign + Copy + fmt::Debug, Kind: CoordinateSpace> SaturatingSub
    for Extent<N, Kind>
{
    #[inline]
    fn saturating_sub(&self, rhs: &Self) -> Self {
        debug_assert!(
            self.w >= rhs.w && self.h >= rhs.h,
            "Attempting to subtract bigger from smaller size: {:?} - {:?}",
            (&self.w, &self.h),
            (&rhs.w, &rhs.h),
        );

        Extent::new(self.w.saturating_sub(&rhs.w), self.h.saturating_sub(&rhs.h))
    }
}

impl<N: Div<Output = N> + Copy, Kind: CoordinateSpace> Div<Extent<N, Kind>> for Extent<N, Kind> {
    type Output = Scale<N>;

    #[inline]
    /// Caculate the scale factor from one extent to another
    fn div(self, rhs: Extent<N, Kind>) -> Self::Output {
        Scale {
            x: self.w / rhs.w,
            y: self.h / rhs.h,
        }
    }
}

impl<N: Sign + Zero + Copy, Kind: CoordinateSpace> Default for Extent<N, Kind> {
    fn default() -> Self {
        Extent::new(Zero::zero(), Zero::zero())
    }
}

impl<N: Add<Output = N> + Copy, Kind: CoordinateSpace> Add<Extent<N, Kind>> for Point<N, Kind> {
    type Output = Point<N, Kind>;

    #[inline]
    fn add(self, other: Extent<N, Kind>) -> Point<N, Kind> {
        Point::new(self.x + other.w, self.y + other.h)
    }
}

impl<N: Sub<Output = N> + Copy, Kind: CoordinateSpace> Sub<Extent<N, Kind>> for Point<N, Kind> {
    type Output = Point<N, Kind>;

    #[inline]
    fn sub(self, other: Extent<N, Kind>) -> Point<N, Kind> {
        Point::new(self.x - other.w, self.y - other.h)
    }
}

/// A rectangle defined by its top-left corner and dimensions
///
/// Operations on rectangles are saturating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Rectangle<N, Kind: CoordinateSpace> {
    /// Location of the top-left corner of the rectangle
    pub loc:  Point<N, Kind>,
    /// Extent of the rectangle, as (width, height)
    pub size: Extent<N, Kind>,
}

impl<N: Sign + Zero + Copy, Kind: CoordinateSpace> Default for Rectangle<N, Kind> {
    fn default() -> Self {
        Rectangle {
            loc:  Point::default(),
            size: Extent::default(),
        }
    }
}

impl<N, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Convert the underlying numerical type to another
    #[inline]
    pub fn to<M: Copy + Sign + 'static>(self) -> Rectangle<M, Kind>
    where
        N: AsPrimitive<M>,
    {
        Rectangle {
            loc:  self.loc.to(),
            size: self.size.to(),
        }
    }
}

impl<N: SaturatingMul + Sign + Copy, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Upscale this [`Rectangle`] by the supplied [`Scale`]
    #[inline]
    pub fn saturating_mul(self, scale: Scale<N>) -> Rectangle<N, Kind> {
        Rectangle {
            loc:  self.loc.saturating_mul(scale),
            size: self.size.saturating_mul(scale),
        }
    }
}

impl<N: Div<Output = N> + Sign + Copy, Kind: CoordinateSpace> Div<Scale<N>> for Rectangle<N, Kind> {
    type Output = Rectangle<N, Kind>;

    /// Upscale this [`Rectangle`] by the supplied [`Scale`]
    #[inline]
    fn div(self, scale: Scale<N>) -> Rectangle<N, Kind> {
        Rectangle {
            loc:  self.loc / scale,
            size: self.size / scale,
        }
    }
}

impl<N: Zero + Eq, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Check if this [`Rectangle`] is empty
    ///
    /// Returns true if either the width or the height
    /// of the [`Extent`] is zero
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.size.is_empty()
    }
}

impl<N: Approx + Sign + Sub<Output = N> + Add<Output = N> + Copy, Kind: CoordinateSpace>
    Rectangle<N, Kind>
{
    /// Round the rectangle's position and size to the nearest integer.
    #[inline]
    pub fn round(self) -> Rectangle<N, Kind> {
        Rectangle {
            loc:  self.loc.round(),
            size: self.size.round(),
        }
    }

    /// Shrink the rectangle to the biggest integer sized and positioned
    /// rectangle within the original rectangle.
    #[inline]
    pub fn shrink(self) -> Rectangle<N, Kind> {
        Rectangle::from_extemities(self.loc.ceil(), (self.loc + self.size).floor())
    }

    /// Expand the rectangle to the smallest integer sized and positioned
    /// rectangle encapsulating the original rectangle.
    #[inline]
    pub fn expand(self) -> Rectangle<N, Kind> {
        Rectangle::from_extemities(self.loc.floor(), (self.loc + self.size).ceil())
    }
}
impl<N: Copy, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Create a new [`Rectangle`] from the coordinates of its top-left corner
    /// and its dimensions
    #[inline]
    pub fn from_loc_and_size(loc: Point<N, Kind>, size: Extent<N, Kind>) -> Self {
        Rectangle { loc, size }
    }
}

impl<N: Sign + Sub<Output = N> + Copy, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Create a new [`Rectangle`] from the coordinates of its top-left corner
    /// and its bottom-right corner
    #[inline]
    pub fn from_extemities(topleft: Point<N, Kind>, bottomright: Point<N, Kind>) -> Self {
        let delta = bottomright - topleft;
        Rectangle {
            loc:  topleft,
            size: Extent::new(delta.x, delta.y),
        }
    }
}
impl<N: Sub<Output = N> + Ord, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Checks whether given [`Point`] is inside the rectangle
    #[inline]
    pub fn contains(self, p: Point<N, Kind>) -> bool {
        (p.x >= self.loc.x) &&
            (p.x - self.loc.x <= self.size.w) &&
            (p.y >= self.loc.y) &&
            (p.y - self.loc.y <= self.size.h)
    }

    /// Checks whether given [`Rectangle`] is inside the rectangle
    ///
    /// This includes rectangles with the same location and size
    #[inline]
    pub fn contains_rect<R: Into<Rectangle<N, Kind>>>(self, rect: R) -> bool {
        let r: Rectangle<N, Kind> = rect.into();
        r.loc.x >= self.loc.x &&
            r.loc.y >= self.loc.y &&
            r.size.w <= self.size.w && // these two checks are to prevent
            r.size.h <= self.size.h && // substraction underflow
            r.loc.x - self.loc.x <= self.size.w - r.size.w &&
            r.loc.y - self.loc.y <= self.size.h - r.size.h
    }
}
impl<N: Ord + Sign + Sub<Output = N> + Zero + Copy, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Compute the bounding box of a given set of points
    pub fn bounding_box(points: impl IntoIterator<Item = Point<N, Kind>>) -> Self {
        let ret = points.into_iter().fold(None, |acc, point| match acc {
            None => Some((point, point)),
            Some((min_point, max_point)) => Some((
                Point::new(point.x.min(min_point.x), point.y.min(min_point.y)),
                Point::new(point.x.max(max_point.x), point.y.max(max_point.y)),
            )),
        });

        match ret {
            None => Rectangle::from_extemities(
                Point::new(N::zero(), N::zero()),
                Point::new(N::zero(), N::zero()),
            ),
            Some((min_point, max_point)) => Rectangle::from_extemities(min_point, max_point),
        }
    }

    /// Merge two [`Rectangle`] by producing the smallest rectangle that
    /// contains both
    #[inline]
    pub fn merge(self, other: Self) -> Self {
        Self::bounding_box([
            self.loc,
            self.loc + self.size,
            other.loc,
            other.loc + other.size,
        ])
    }
}

impl<N: SaturatingAdd + Ord, Kind: CoordinateSpace> Rectangle<N, Kind> {
    /// Checks whether a given [`Rectangle`] overlaps with this one
    #[inline]
    pub fn overlaps(self, other: &Rectangle<N, Kind>) -> bool {
        self.loc.x <= other.loc.x.saturating_add(&other.size.w) &&
            other.loc.x <= self.loc.x.saturating_add(&self.size.w) &&
            self.loc.y <= other.loc.y.saturating_add(&other.size.h) &&
            other.loc.y <= self.loc.y.saturating_add(&self.size.h)
    }
}

impl<N: Sign + Copy, Kind: CoordinateSpace> coords::Map for Rectangle<N, Kind> {
    type Output<Kind2> = Rectangle<N, Kind2> where Kind2: CoordinateSpace;

    /// Convert this physical rectangle to logical coordinate space according to
    /// given scale factor
    #[inline]
    fn map<Kind2: CoordinateSpace>(
        self,
        f: impl FnOnce(Rectangle<N, Kind2>) -> Rectangle<N, Kind2>,
    ) -> Rectangle<N, Kind2> {
        let mapped = Rectangle {
            loc:  Point::new(self.loc.x, self.loc.y),
            size: Extent::new(self.size.w, self.size.h),
        };
        f(mapped)
    }
}

impl<N> Rectangle<N, Buffer> {
    // TODO: Convert this physical rectangle to logical coordinate space according
    // to given scale factor
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy, Default)]
/// Possible transformations to two-dimensional planes
pub enum Transform {
    /// Identity transformation (plane is unaltered when applied)
    #[default]
    Normal,
    /// Plane is rotated by 90 degrees
    _90,
    /// Plane is rotated by 180 degrees
    _180,
    /// Plane is rotated by 270 degrees
    _270,
    /// Plane is flipped vertically
    Flipped,
    /// Plane is flipped vertically and rotated by 90 degrees
    Flipped90,
    /// Plane is flipped vertically and rotated by 180 degrees
    Flipped180,
    /// Plane is flipped vertically and rotated by 270 degrees
    Flipped270,
}

/// Coordinate system is irrelevant to other types, but it does affect how
/// transformations are performed. Here we define the coordinate system to be X
/// to the left, Y down, origin at top left.
impl Transform {
    /// Inverts the transformation
    pub fn invert(&self) -> Transform {
        match self {
            Transform::Normal => Transform::Normal,
            Transform::Flipped => Transform::Flipped,
            Transform::_90 => Transform::_270,
            Transform::_180 => Transform::_180,
            Transform::_270 => Transform::_90,
            Transform::Flipped90 => Transform::Flipped90,
            Transform::Flipped180 => Transform::Flipped180,
            Transform::Flipped270 => Transform::Flipped270,
        }
    }

    /// Get the coordinates of a point inside an extent, after applying the
    /// transformation to the extent. The Extent is placed at (0, 0) to (w,
    /// h), and transformation is applied so that the upper-left corner of
    /// the resulting extent is at (0, 0).
    pub fn transform_point_in<N: Sub<Output = N> + Copy, Kind: CoordinateSpace>(
        &self,
        point: Point<N, Kind>,
        area: &Extent<N, Kind>,
    ) -> Point<N, Kind> {
        let (x, y) = match *self {
            Transform::Normal => (point.x, point.y),
            Transform::_90 => (point.y, area.w - point.x),
            Transform::_180 => (area.w - point.x, area.h - point.y),
            Transform::_270 => (area.h - point.y, point.x),
            Transform::Flipped => (area.w - point.x, point.y),
            Transform::Flipped90 => (point.y, point.x),
            Transform::Flipped180 => (point.x, area.h - point.y),
            Transform::Flipped270 => (area.h - point.y, area.w - point.x),
        };
        Point::new(x, y)
    }

    /// Transformed size after applying this transformation.
    pub fn transform_size<N: Sign + Copy, Kind: CoordinateSpace>(
        &self,
        size: Extent<N, Kind>,
    ) -> Extent<N, Kind> {
        if matches!(
            *self,
            Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
        ) {
            Extent::new(size.h, size.w)
        } else {
            size
        }
    }

    /// Transforms a rectangle inside an area of a given size by applying this
    /// transformation.
    #[inline]
    pub fn transform_rect_in<
        N: Sub<Output = N> + Sign + Copy + fmt::Debug,
        Kind: CoordinateSpace,
    >(
        &self,
        rect: Rectangle<N, Kind>,
        area: &Extent<N, Kind>,
    ) -> Rectangle<N, Kind> {
        let size = self.transform_size(rect.size);
        // Transform the upper-left corner.
        let loc = self.transform_point_in(rect.loc, area);
        eprintln!("loc: {loc:?}, {self:?}");
        // After transformation, the upper-left is no longer the upper-left, so find the
        // coordinate of the new upper-left.
        let (x, y) = match *self {
            Transform::Normal | Transform::Flipped90 => (loc.x, loc.y),
            Transform::_90 | Transform::Flipped180 => (loc.x, loc.y - size.h),
            Transform::_180 | Transform::Flipped270 => (loc.x - size.w, loc.y - size.h),
            Transform::_270 | Transform::Flipped => (loc.x - size.w, loc.y),
        };

        Rectangle::from_loc_and_size(Point::new(x, y), size)
    }

    /// Returns true if the transformation would flip contents
    #[inline]
    pub fn flipped(&self) -> bool {
        !matches!(
            self,
            Transform::Normal | Transform::_90 | Transform::_180 | Transform::_270
        )
    }

    /// Returns the angle (in degrees) of the transformation
    #[inline]
    pub fn degrees(&self) -> u32 {
        match self {
            Transform::Normal | Transform::Flipped => 0,
            Transform::_90 | Transform::Flipped90 => 90,
            Transform::_180 | Transform::Flipped180 => 180,
            Transform::_270 | Transform::Flipped270 => 270,
        }
    }
}

impl TryFrom<(bool, u32)> for Transform {
    type Error = ();

    #[inline]
    fn try_from((flipped, degrees): (bool, u32)) -> Result<Transform, ()> {
        match (flipped, degrees) {
            (false, 0) => Ok(Transform::Normal),
            (false, 90) => Ok(Transform::_90),
            (false, 180) => Ok(Transform::_180),
            (false, 270) => Ok(Transform::_270),
            (true, 0) => Ok(Transform::Flipped),
            (true, 90) => Ok(Transform::Flipped90),
            (true, 180) => Ok(Transform::Flipped180),
            (true, 270) => Ok(Transform::Flipped270),
            _ => Err(()),
        }
    }
}

impl std::ops::Mul for Transform {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        // Rotation * flip = flip * -Rotation
        // self * other = flip1 * rotation1 * flip2 * rotation2 =
        //                flip1 * flip2 * (-1? * rotation1) * rotation2
        // the negative 1 factor comes in if flip2 is not identity.
        let flipped = self.flipped() ^ other.flipped();
        let self_degrees = if other.flipped() {
            360 - self.degrees()
        } else {
            self.degrees()
        };
        let degrees = (self_degrees + other.degrees()) % 360;
        (flipped, degrees).try_into().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::{Extent, Point, Rectangle, Screen, Transform};

    #[test]
    fn transform_rect_ident() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::Normal;

        assert_eq!(rect, transform.transform_rect_in(rect, &area))
    }

    #[test]
    fn transform_rect_90() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::_90;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(20, 30), Extent::new(40, 30)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_180() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::_180;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(30, 30), Extent::new(30, 40)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_270() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::_270;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(30, 10), Extent::new(40, 30)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_f() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::Flipped;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(30, 20), Extent::new(30, 40)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_f90() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 80);
        let transform = Transform::Flipped90;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(20, 10), Extent::new(40, 30)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_f180() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::Flipped180;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(10, 30), Extent::new(30, 40)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn transform_rect_f270() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let area = Extent::new(70, 90);
        let transform = Transform::Flipped270;

        assert_eq!(
            Rectangle::from_loc_and_size(Point::new(30, 30), Extent::new(40, 30)),
            transform.transform_rect_in(rect, &area)
        )
    }

    #[test]
    fn rectangle_contains_rect_itself() {
        let rect =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        assert!(rect.contains_rect(rect));
    }

    #[test]
    fn rectangle_contains_rect_outside() {
        let first =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let second =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(41, 61), Extent::new(30, 40));
        assert!(!first.contains_rect(second));
    }

    #[test]
    fn rectangle_contains_rect_extends() {
        let first =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 40));
        let second =
            Rectangle::<i32, Screen>::from_loc_and_size(Point::new(10, 20), Extent::new(30, 45));
        assert!(!first.contains_rect(second));
    }
}
