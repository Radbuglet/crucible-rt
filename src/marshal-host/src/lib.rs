use std::{any::type_name, marker::PhantomData};

use anyhow::Context;
use bytemuck::Pod;

// === Re-Exports === //

pub use crt_marshal::*;

// === Heap Parsing === //

pub const fn size_of_32<T>() -> u32 {
    struct SizeOf<T>(PhantomData<fn() -> T>);

    impl<T> SizeOf<T> {
        const SIZE: u32 = {
            let size = std::mem::size_of::<T>();
            if size > u32::MAX as usize {
                panic!("structure is too big");
            }

            size as u32
        };
    }

    <SizeOf<T>>::SIZE
}

pub const fn align_of_32<T>() -> u32 {
    struct AlignOf<T>(PhantomData<fn() -> T>);

    impl<T> AlignOf<T> {
        const SIZE: u32 = {
            let size = std::mem::align_of::<T>();
            if size > u32::MAX as usize {
                panic!("structure is too big");
            }

            size as u32
        };
    }

    <AlignOf<T>>::SIZE
}

pub trait MemoryRead {
    fn as_slice(&self) -> &[u8];

    fn load_range(&self, base: u32, len: u32) -> anyhow::Result<&[u8]> {
        self.as_slice()
            .get(base as usize..)
            .and_then(|s| s.get(..len as usize))
            .with_context(|| {
                format!(
                    "failed to read memory range from {base} to {len} (memory size: {})",
                    self.as_slice().len()
                )
            })
    }

    fn load_struct_raw<T: Pod>(&self, ptr: u32) -> anyhow::Result<&T> {
        bytemuck::try_from_bytes(self.load_range(ptr, size_of_32::<T>())?).map_err(|err| {
            anyhow::anyhow!(
                "failed to parse object (ty: {}, base: {ptr}): {err}",
                type_name::<T>()
            )
        })
    }

    fn load_slice_raw<T: Pod>(&self, base: u32, len: u32) -> anyhow::Result<&[T]> {
        bytemuck::try_cast_slice(
            self.load_range(
                base,
                len.checked_mul(size_of_32::<T>())
                    .context("slice is too big")?,
            )?,
        )
        .map_err(|err| {
            anyhow::anyhow!(
                "failed to parse slice (ty: {}, base: {base}, len: {len}): {err}",
                type_name::<T>()
            )
        })
    }

    fn load_str_raw(&self, base: u32, len: u32) -> anyhow::Result<&str> {
        self.load_range(base, len)
            .and_then(|data| std::str::from_utf8(data).context("invalid UTF-8"))
    }

    fn load_struct<T: Pod>(&self, ptr: WasmPtr<T>) -> anyhow::Result<&T> {
        self.load_struct_raw(ptr.addr().get())
    }

    fn load_slice<T: Pod>(&self, ptr: WasmSlice<T>) -> anyhow::Result<&[T]> {
        self.load_slice_raw(ptr.base.addr().get(), ptr.len.get())
    }

    fn load_str(&self, ptr: WasmStr) -> anyhow::Result<&str> {
        self.load_str_raw(ptr.0.base.addr().get(), ptr.0.len.get())
    }
}

impl MemoryRead for [u8] {
    fn as_slice(&self) -> &[u8] {
        self
    }
}

pub trait MemoryWrite: MemoryRead {
    fn as_slice_mut(&mut self) -> &mut [u8];

    fn load_range_mut(&mut self, base: u32, len: u32) -> anyhow::Result<&mut [u8]> {
        let mem_len = self.as_slice().len();
        self.as_slice_mut()
            .get_mut(base as usize..)
            .and_then(|s| s.get_mut(..len as usize))
            .with_context(|| {
                format!("failed to read memory range from {base} to {len} (memory size: {mem_len})")
            })
    }

    fn write_range_mut(&mut self, base: u32, data: &[u8]) -> anyhow::Result<()> {
        self.load_range_mut(base, u32::try_from(data.len()).context("slice is too big")?)?
            .copy_from_slice(data);

        Ok(())
    }

    fn write_struct<T: Pod>(&mut self, base: WasmPtr<T>, data: &T) -> anyhow::Result<()> {
        self.write_range_mut(base.addr().get(), bytemuck::bytes_of(data))
    }

    fn write_slice<'a, T: Pod>(
        &mut self,
        base: WasmPtr<T>,
        items: impl IntoIterator<Item = &'a T>,
    ) -> anyhow::Result<u32> {
        let mut offset = base.addr().get();
        let mut count = 0;

        for item in items {
            self.write_struct(WasmPtr::new(offset.into()), item)?;

            offset = offset
                .checked_add(size_of_32::<T>())
                .context("wrote too many elements into memory")?;

            count += 1;
        }

        Ok(count)
    }
}

impl MemoryWrite for [u8] {
    fn as_slice_mut(&mut self) -> &mut [u8] {
        self
    }
}

// === Host-Side Function Handling === //

// HostSideMarshaledFunc
pub trait HostSideMarshaledFunc<D, Params, Results>: Sized {
    type PrimParams<'a>;
    type PrimResults;

    #[rustfmt::skip]
    fn wrap_host(self) ->
        impl for<'a> wasmtime::IntoFunc<D, Self::PrimParams<'a>, Self::PrimResults>;
}

macro_rules! impl_func_ty {
    ($($ty:ident)*) => {
        impl<D, F, Ret, $($ty: MarshaledTy,)*> HostSideMarshaledFunc<D, ($($ty,)*), Ret> for F
        where
            D: 'static,
            Ret: MarshaledTyList,
            F: 'static + Send + Sync + Fn(wasmtime::Caller<'_, D>, $($ty,)*) -> anyhow::Result<Ret>,
        {
            type PrimParams<'a> = (wasmtime::Caller<'a, D>, $(<$ty as MarshaledTy>::Prim,)*);
            type PrimResults = anyhow::Result<Ret::Prims>;

            #[allow(non_snake_case, unused_mut)]
            fn wrap_host(self) -> impl for<'a> wasmtime::IntoFunc<D, Self::PrimParams<'a>, Self::PrimResults> {
                move |mut caller: wasmtime::Caller<'_, D>, $($ty: <$ty as MarshaledTy>::Prim,)*| {
                    self(caller, $(<$ty>::from_prim($ty).context("failed to parse argument")?),*)
                        .map(MarshaledTyList::into_prims)
                }
            }
        }
    };
}

impl_variadic!(impl_func_ty);

// `bind_to_linker`
pub fn bind_to_linker<'l, F, T, Params, Results>(
    linker: &'l mut wasmtime::Linker<T>,
    module: &str,
    name: &str,
    func: F,
) -> anyhow::Result<&'l mut wasmtime::Linker<T>>
where
    F: HostSideMarshaledFunc<T, Params, Results>,
{
    linker.func_wrap(module, name, func.wrap_host())
}

// === Guest-Side Function Handling === //

pub struct WasmFuncRef<A, R = ()>(pub wasmtime::TypedFunc<A::Prims, R::Prims>)
where
    A: MarshaledTyList,
    R: MarshaledTyList;

impl<A, R> Copy for WasmFuncRef<A, R>
where
    A: MarshaledTyList,
    R: MarshaledTyList,
{
}

impl<A, R> Clone for WasmFuncRef<A, R>
where
    A: MarshaledTyList,
    R: MarshaledTyList,
{
    fn clone(&self) -> Self {
        *self
    }
}

impl<A, R> WasmFuncRef<A, R>
where
    A: MarshaledTyList,
    R: MarshaledTyList,
{
    pub fn decode<T: StoreHasTable>(
        mut cx: impl wasmtime::AsContextMut<Data = T>,
        idx: WasmFunc<A, R>,
    ) -> anyhow::Result<Self> {
        let table = cx.as_context().data().func_table();
        let func = table
            .get(&mut cx, idx.addr().get())
            .with_context(|| format!("failed to resolve table entry with index {idx:?}"))?;

        let func = func
            .funcref()
            .flatten()
            .context("entry is not a `funcref`")?;

        Ok(Self(func.typed(&cx).context("func has wrong type")?))
    }

    pub fn call<D>(
        &self,
        mut store: impl wasmtime::AsContextMut<Data = D>,
        args: A,
    ) -> anyhow::Result<R> {
        R::from_prims(self.0.call(&mut store, A::into_prims(args))?)
            .context("failed to deserialize results")
    }
}

// === WasmDynamic Extensions === //

pub trait WasmDynamicExt {
    type Vtable: 'static;

    fn get_vtable(self, mem: &(impl ?Sized + MemoryRead)) -> anyhow::Result<&Self::Vtable>
    where
        Self::Vtable: bytemuck::Pod;

    fn run_dtor<S>(self, cx: impl wasmtime::AsContextMut<Data = S>) -> anyhow::Result<()>
    where
        S: StoreHasMemory + StoreHasTable;
}

impl<V: 'static> WasmDynamicExt for WasmDynamic<V> {
    type Vtable = V;

    fn get_vtable(self, mem: &(impl ?Sized + MemoryRead)) -> anyhow::Result<&Self::Vtable>
    where
        Self::Vtable: bytemuck::Pod,
    {
        mem.load_struct(mem.load_struct(self.0.meta)?.vtable)
    }

    fn run_dtor<S>(self, mut cx: impl wasmtime::AsContextMut<Data = S>) -> anyhow::Result<()>
    where
        S: StoreHasMemory + StoreHasTable,
    {
        let table = cx.main_memory().load_struct(self.0.meta)?;
        if table.needs_drop.get() != 0 {
            let dtor = table.dtor;
            let dtor = WasmFuncRef::decode(&mut cx, dtor)?;
            dtor.call(cx, (self.0.base, self.0.meta))?;
        }
        Ok(())
    }
}

pub trait WasmDynamicFuncExt {
    type Args;
    type Res;

    fn call<S>(
        self,
        cx: impl wasmtime::AsContextMut<Data = S>,
        args: Self::Args,
    ) -> anyhow::Result<Self::Res>
    where
        S: StoreHasMemory + StoreHasTable;
}

impl<A, R> WasmDynamicFuncExt for WasmDynamicFunc<A, R>
where
    A: ExtensibleMarshaledTyList,
    R: MarshaledTyList,
{
    type Args = A;
    type Res = R;

    fn call<S>(
        self,
        mut cx: impl wasmtime::AsContextMut<Data = S>,
        args: Self::Args,
    ) -> anyhow::Result<Self::Res>
    where
        S: StoreHasMemory + StoreHasTable,
    {
        let func = *self.get_vtable(cx.main_memory())?;
        let func = WasmFuncRef::decode(&mut cx, func.0)?;
        func.call(cx, args.push_on_first(self.0.base))
    }
}

// === StoreHasTable === //

pub trait StoreHasTable {
    fn func_table(&self) -> wasmtime::Table;
}

// === StoreHasMemory === //

pub trait StoreHasMemory {
    fn main_memory(&self) -> wasmtime::Memory;

    fn alloc_func(&self) -> WasmFuncRef<(u32, u32), WasmPtr<()>>;
}

pub trait ContextMemoryExt: Sized + wasmtime::AsContextMut<Data = Self::Data_> {
    type Data_: StoreHasMemory;

    fn split_main_memory(&mut self) -> (&mut [u8], &mut Self::Data_) {
        let memory = self.as_context_mut().data().main_memory();
        memory.data_and_store_mut(self)
    }

    fn main_memory(&mut self) -> &mut [u8] {
        self.split_main_memory().0
    }

    fn alloc(&mut self, size: u32, align: u32) -> anyhow::Result<WasmPtr<()>> {
        let alloc = self.as_context_mut().data().alloc_func();
        alloc.call(self, (size, align))
    }

    fn alloc_struct<T: Pod>(&mut self, value: &T) -> anyhow::Result<WasmPtr<T>> {
        let ptr = self
            .alloc(size_of_32::<T>(), align_of_32::<T>())
            .map(|v| WasmPtr::<T>::new(v.addr()))?;

        let (memory, _) = self.split_main_memory();
        memory.write_struct(ptr, value)?;
        Ok(ptr)
    }

    fn alloc_slice<'a, T: Pod>(
        &mut self,
        values: impl ExactSizeIterator<Item = &'a T>,
    ) -> anyhow::Result<WasmSlice<T>> {
        let len = u32::try_from(values.len()).context("too many elements in slice")?;
        let size = size_of_32::<T>()
            .checked_mul(len)
            .context("slice is too big")?;

        let base = self
            .alloc(size, align_of_32::<T>())
            .map(|v| WasmPtr::<T>::new(v.addr()))?;

        let (memory, _) = self.split_main_memory();
        memory.write_slice(base, values)?;

        Ok(WasmSlice {
            base,
            len: len.into(),
        })
    }

    fn alloc_str(&mut self, data: &str) -> anyhow::Result<WasmStr> {
        self.alloc_slice(data.as_bytes().iter()).map(WasmStr)
    }
}

impl<T: wasmtime::AsContextMut> ContextMemoryExt for T
where
    T::Data: StoreHasMemory,
{
    type Data_ = T::Data;
}
