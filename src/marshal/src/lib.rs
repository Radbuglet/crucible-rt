#![no_std]

use core::{fmt, marker::PhantomData, ptr::NonNull};

use bytemuck::{Pod, Zeroable};

// === Helpers === //

#[macro_export]
#[doc(hidden)]
macro_rules! impl_variadic {
    ($target:path) => {
        impl_variadic!($target; V1 V2 V3 V4 V5 V6 V7 V8 V9 V10 V11 V12);
    };
    ($target:path; $($first:ident $($remaining:ident)*)?) => {
        $target!($($first $($remaining)*)?);
        $(impl_variadic!($target; $($remaining)*);)?
    };
}

// === WasmPrimitive === //

mod wasm_primitive {
    #[cfg(feature = "wasmtime")]
    pub trait Sealed: wasmtime::WasmTy {}

    #[cfg(not(feature = "wasmtime"))]
    pub trait Sealed {}
}

pub trait WasmPrimitive: wasm_primitive::Sealed {}

macro_rules! impl_wasm_primitive {
    ($($ty:ty),*$(,)?) => {$(
        impl wasm_primitive::Sealed for $ty {}
        impl WasmPrimitive for $ty {}
    )*};
}

impl_wasm_primitive!(u32, i32, u64, i64, f32, f64);

// === WasmPrimitiveList === //

mod wasm_primitive_list {
    #[cfg(feature = "wasmtime")]
    pub trait Sealed: wasmtime::WasmRet + wasmtime::WasmResults + wasmtime::WasmParams {}

    #[cfg(not(feature = "wasmtime"))]
    pub trait Sealed {}
}

pub trait WasmPrimitiveList: wasm_primitive_list::Sealed {}

impl<T: WasmPrimitive> wasm_primitive_list::Sealed for T {}

impl<T: WasmPrimitive> WasmPrimitiveList for T {}

macro_rules! impl_wasm_primitive_list {
    ($($para:ident)*) => {
        impl<$($para: WasmPrimitive),*> wasm_primitive_list::Sealed for ($($para,)*) {}

        impl<$($para: WasmPrimitive),*> WasmPrimitiveList for ($($para,)*) {}
    };
}

impl_variadic!(impl_wasm_primitive_list);

// === MarshaledTy === //

pub trait MarshaledTy: Sized {
    type Prim: WasmPrimitive;

    fn into_prim(me: Self) -> Self::Prim;

    fn from_prim(me: Self::Prim) -> Option<Self>;
}

macro_rules! impl_func_ty {
    ($($ty:ty => $prim:ty),*$(,)?) => {$(
        impl MarshaledTy for $ty {
            type Prim = $prim;

            fn into_prim(me: Self) -> Self::Prim {
                me.into()
            }

            fn from_prim(me: Self::Prim) -> Option<Self> {
                Self::try_from(me).ok()
            }
        }
    )*};
}

impl_func_ty!(
    u8 => u32,
    u16 => u32,
    u32 => u32,
    i8 => i32,
    i16 => i32,
    i32 => i32,
    u64 => u64,
    i64 => i64,
    char => u32,
);

impl MarshaledTy for bool {
    type Prim = u32;

    fn into_prim(me: Self) -> Self::Prim {
        me as u32
    }

    fn from_prim(me: Self::Prim) -> Option<Self> {
        match me {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }
}

// === MarshaledResults === //

pub trait MarshaledTyList: Sized {
    type Prims: WasmPrimitiveList;

    fn into_prims(me: Self) -> Self::Prims;

    fn from_prims(me: Self::Prims) -> Option<Self>;
}

impl<T: MarshaledTy> MarshaledTyList for T {
    type Prims = T::Prim;

    fn into_prims(me: Self) -> Self::Prims {
        T::into_prim(me)
    }

    fn from_prims(me: Self::Prims) -> Option<Self> {
        T::from_prim(me)
    }
}

macro_rules! impl_marshaled_res_ty {
    ($($para:ident)*) => {
        impl<$($para: MarshaledTy,)*> MarshaledTyList for ($($para,)*) {
            type Prims = ($(<$para as MarshaledTy>::Prim,)*);

            #[allow(clippy::unused_unit, non_snake_case)]
            fn into_prims(($($para,)*): Self) -> Self::Prims {
                ( $(MarshaledTy::into_prim($para),)* )
            }

            #[allow(non_snake_case)]
            fn from_prims(($($para,)*): Self::Prims) -> Option<Self> {
                Some(( $(MarshaledTy::from_prim($para)?,)* ))
            }
        }
    };
}

impl_variadic!(impl_marshaled_res_ty);

// === Little Endian Types === //

macro_rules! define_le {
    ($($name:ident $ty:ty),*$(,)?) => {$(
        #[derive(Copy, Clone, Pod, Zeroable)]
        #[repr(transparent)]
        pub struct $name($ty);

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

		impl fmt::Binary for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

		impl fmt::LowerHex for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

		impl fmt::LowerExp for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

		impl fmt::Octal for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

		impl fmt::UpperExp for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(f)
            }
        }

        impl $name {
            pub fn new(value: $ty) -> Self {
                Self(value.to_le())
            }

            pub fn get(self) -> $ty {
                <$ty>::from_le(self.0)
            }

            pub fn set(&mut self, v: $ty) {
                *self = Self::new(v)
            }

            pub fn update<R>(&mut self, f: impl FnOnce(&mut $ty) -> R) -> R {
                let mut ne = self.get();
                let res = f(&mut ne);
                self.set(ne);
                res
            }

            pub fn map(self, f: impl FnOnce($ty) -> $ty) -> Self {
                f(self.into()).into()
            }
        }

        impl From<$ty> for $name {
            fn from(value: $ty) -> Self {
                Self::new(value)
            }
        }

        impl From<$name> for $ty {
            fn from(value: $name) -> Self {
                value.get()
            }
        }

        impl MarshaledTy for $name {
            type Prim = <$ty as MarshaledTy>::Prim;

            fn into_prim(me: Self) -> Self::Prim {
                <$ty>::into_prim(me.get())
            }

            fn from_prim(me: Self::Prim) -> Option<Self> {
                <$ty>::from_prim(me).map(Self::new)
            }
        }
    )*};
}

define_le! {
    LeI16 i16,
    LeU16 u16,
    LeI32 i32,
    LeU32 u32,
    LeI64 i64,
    LeU64 u64,
}

// === Pointers === //

// WasmPtr
#[repr(transparent)]
pub struct WasmPtr<T: 'static> {
    pub _ty: PhantomData<fn() -> T>,
    pub addr: LeU32,
}

impl<T> fmt::Debug for WasmPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.addr.get() as usize as *const T).fmt(f)
    }
}

impl<T> fmt::Pointer for WasmPtr<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.addr.get() as usize as *const T).fmt(f)
    }
}

impl<T> Copy for WasmPtr<T> {}

impl<T> Clone for WasmPtr<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> MarshaledTy for WasmPtr<T> {
    type Prim = u32;

    fn into_prim(me: Self) -> Self::Prim {
        MarshaledTy::into_prim(me.addr)
    }

    fn from_prim(me: Self::Prim) -> Option<Self> {
        MarshaledTy::from_prim(me).map(|addr| Self {
            _ty: PhantomData,
            addr,
        })
    }
}

unsafe impl<T> Pod for WasmPtr<T> {}
unsafe impl<T> Zeroable for WasmPtr<T> {}

// WasmSlice
pub struct WasmSlice<T: 'static> {
    pub base: WasmPtr<T>,
    pub len: LeU32,
}

impl<T> fmt::Debug for WasmSlice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WasmSlice")
            .field("base", &self.base)
            .field("len", &self.len)
            .finish()
    }
}

impl<T> Copy for WasmSlice<T> {}

impl<T> Clone for WasmSlice<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Copy, Clone, Pod, Zeroable)]
#[repr(C)]
struct WasmSliceRaw(u32, u32);

impl<T> MarshaledTy for WasmSlice<T> {
    type Prim = u64;

    fn into_prim(me: Self) -> Self::Prim {
        bytemuck::cast(WasmSliceRaw(me.base.addr.get(), me.len.get()))
    }

    fn from_prim(me: Self::Prim) -> Option<Self> {
        let WasmSliceRaw(base, len) = bytemuck::cast::<_, WasmSliceRaw>(me);

        Some(Self {
            base: WasmPtr {
                _ty: PhantomData,
                addr: LeU32::new(base),
            },
            len: LeU32::new(len),
        })
    }
}

unsafe impl<T: 'static> Pod for WasmSlice<T> {}
unsafe impl<T: 'static> Zeroable for WasmSlice<T> {}

// WasmStr
#[derive(Debug, Copy, Clone, Pod, Zeroable)]
#[repr(C)]
pub struct WasmStr(pub WasmSlice<u8>);

impl MarshaledTy for WasmStr {
    type Prim = u64;

    fn into_prim(me: Self) -> Self::Prim {
        WasmSlice::into_prim(me.0)
    }

    fn from_prim(me: Self::Prim) -> Option<Self> {
        Some(WasmStr(WasmSlice::from_prim(me).unwrap()))
    }
}

// === Guest Constructors === //

// ...as per the suggestion of LegionMammal978 (https://github.com/LegionMammal978). Thanks!
fn slice_len<T>(mut ptr: *const [T]) -> usize {
    // This is done to avoid https://github.com/rust-lang/rust/issues/120440.
    if ptr.is_null() {
        ptr = ptr.wrapping_byte_add(1);
    }
    let ptr = unsafe { NonNull::new_unchecked(ptr.cast_mut()) };
    ptr.len()
}

fn usize_to_u32(v: usize) -> u32 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = v;
        panic!("attempted to call guest function on non-guest platform");
    }

    #[cfg(target_arch = "wasm32")]
    {
        v as u32
    }
}

impl<T> WasmPtr<T> {
    pub fn new_guest(ptr: *const T) -> Self {
        Self {
            _ty: PhantomData,
            addr: LeU32::new(usize_to_u32(ptr as usize)),
        }
    }
}

impl<T> WasmSlice<T> {
    pub fn new_guest(ptr: *const [T]) -> Self {
        Self {
            base: WasmPtr::new_guest(ptr.cast::<T>()),
            len: LeU32::new(usize_to_u32(slice_len(ptr))),
        }
    }
}

impl WasmStr {
    pub fn new_guest(ptr: *const str) -> Self {
        Self(WasmSlice::new_guest(ptr as *const [u8]))
    }
}

// === Generator === //

#[macro_export]
macro_rules! generate_guest_ffi {
    (
        $(
            $(#[$fn_attr:meta])*
            $vis:vis fn $module:literal.$fn_name:ident(
                $($arg_name:ident: $arg_ty:ty),*
                $(,)?
            ) $( -> $res_ty:ty )?;
        )*
    ) => {$(
        $(#[$fn_attr])*
        $vis unsafe fn $fn_name($($arg_name: $arg_ty),*) $(-> $res_ty)? {
            #[link(wasm_import_module = $module)]
            extern "C" {
                fn $fn_name(
                    $($arg_name: <$arg_ty as $crate::MarshaledTy>::Prim),*
                ) $(-> <$res_ty as $crate::MarshaledTy>::Prim)?;
            }

            $crate::MarshaledResults::from_prims($fn_name(
                $($crate::MarshaledTy::into_prim($arg_name),)*
            ))
            .expect("failed to parse result")
        }
    )*};
}