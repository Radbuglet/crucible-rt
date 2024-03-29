//! Utilities for parsing, writing, interpreting, and applying relocations.

use anyhow::Context;

use crate::util::{BufWriter, ByteCursor, ByteParse, ByteParseList, ByteSliceExt, Leb128WriteExt};

// === Parsing === //

/// Parser for WASM relocation sections as described in the [WebAssembly Object File Linking](linking)
/// informal spec. I'm not exactly sure why the `"linking"` section is included in [`wasmparser`] but
/// the `"reloc."` section isn't.
///
/// [linking]: https://github.com/WebAssembly/tool-conventions/blob/4dd47d204df0c789c23d246bc4496631b5c199c4/Linking.md
#[derive(Debug, Clone)]
pub struct RelocSection<'a> {
    pub target_section: u32,
    pub entry_count: u32,
    pub entries: &'a [u8],
}

impl<'a> ByteParse<'a> for RelocSection<'a> {
    type Out = Self;

    fn parse_naked(buf: &mut ByteCursor<'a>) -> anyhow::Result<Self::Out> {
        let target_section = buf
            .read_var_u32()
            .context("failed to read target section index")?;

        let entry_count = buf
            .read_var_u32()
            .context("failed to read relocation entry array count")?;

        Ok(Self {
            target_section,
            entry_count,
            entries: buf.0,
        })
    }
}

impl<'a> RelocSection<'a> {
    pub fn entries(&self) -> impl Iterator<Item = anyhow::Result<RelocEntry>> + 'a {
        ByteParseList::<RelocEntry>::new(ByteCursor(self.entries)).take(self.entry_count as usize)
    }
}

#[derive(Debug, Copy, Clone)]
pub struct RelocEntry {
    pub ty: RelocEntryType,
    pub offset: u32,
    pub index: u32,
    pub addend: Option<i32>,
}

impl ByteParse<'_> for RelocEntry {
    type Out = Self;

    fn parse_naked(buf: &mut ByteCursor<'_>) -> anyhow::Result<Self::Out> {
        let ty = buf.lookahead_annotated("relocation entry type", |c| {
            RelocEntryType::parse(c.read_u8()?)
        })?;

        let offset = buf.read_var_u32().context("failed to read offset")?;
        let index = buf.read_var_u32().context("failed to read index")?;

        let addend = if ty.has_addend() {
            Some(buf.read_var_i32()?)
        } else {
            None
        };

        Ok(Self {
            ty,
            offset,
            index,
            addend,
        })
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum RelocEntryType {
    FunctionIndexLeb = 0,
    TableIndexSleb = 1,
    TableIndexI32 = 2,
    MemoryAddrLeb = 3,
    MemoryAddrSleb = 4,
    MemoryAddrI32 = 5,
    TypeIndexLeb = 6,
    GlobalIndexLeb = 7,
    FunctionOffsetI32 = 8,
    SectionOffsetI32 = 9,
    EventIndexLeb = 10,
    GlobalIndexI32 = 13,
}

impl RelocEntryType {
    pub fn parse(v: u8) -> anyhow::Result<Self> {
        use RelocEntryType::*;

        Ok(match v {
            0 => FunctionIndexLeb,
            1 => TableIndexSleb,
            2 => TableIndexI32,
            3 => MemoryAddrLeb,
            4 => MemoryAddrSleb,
            5 => MemoryAddrI32,
            6 => TypeIndexLeb,
            7 => GlobalIndexLeb,
            8 => FunctionOffsetI32,
            9 => SectionOffsetI32,
            10 => EventIndexLeb,
            13 => GlobalIndexI32,
            _ => anyhow::bail!("unknown relocation type {v}"),
        })
    }

    pub fn has_addend(self) -> bool {
        use RelocEntryType::*;

        matches!(
            self,
            MemoryAddrLeb | MemoryAddrSleb | MemoryAddrI32 | FunctionOffsetI32 | SectionOffsetI32
        )
    }

    pub fn rewrite_kind(self) -> ScalarRewriteKind {
        use {RelocEntryType::*, ScalarRewriteKind::*};

        match self {
            FunctionIndexLeb => VarU32,
            TableIndexSleb => VarI32,
            TableIndexI32 => U32,
            MemoryAddrLeb => VarU32,
            MemoryAddrSleb => VarI32,
            MemoryAddrI32 => U32,
            TypeIndexLeb => VarU32,
            GlobalIndexLeb => VarU32,
            FunctionOffsetI32 => U32,
            SectionOffsetI32 => U32,
            EventIndexLeb => VarU32,
            GlobalIndexI32 => U32,
        }
    }
}

// === Rewriting === //

pub fn rewrite_relocated<W: BufWriter, C>(
    buf: &[u8],
    writer: &mut W,
    cx: &mut C,
    replacements: impl IntoIterator<Item = (usize, impl Rewriter<W, C>)>,
) -> anyhow::Result<()> {
    // Invariant: `buf_cursor` is always less than or equal to the `buf` length.
    let mut buf_cursor = 0;

    for (reloc_start, rewriter) in replacements {
        // While there are still relocations affecting bytes in our at the end of our buffer...
        debug_assert!(reloc_start >= buf_cursor);

        if reloc_start > buf.len() {
            break;
        }

        // Push the bytes up until the start of the relocation.
        writer.extend(&buf[buf_cursor..reloc_start]);

        // Push the new relocation bytes.
        let reloc_end = reloc_start
            + buf[reloc_start..].try_count_bytes_read(|c| rewriter.rewrite(c, writer, cx))?;

        // Bump the `buf_cursor`
        buf_cursor = reloc_end;
    }

    // Ensure that we write the remaining bytes of our buffer.
    writer.extend(&buf[buf_cursor..]);

    Ok(())
}

pub trait Rewriter<W, C> {
    fn rewrite(self, buf: &mut ByteCursor, writer: &mut W, cx: &mut C) -> anyhow::Result<()>;
}

impl<W, F, C> Rewriter<W, C> for F
where
    F: FnOnce(&mut ByteCursor, &mut W, &mut C) -> anyhow::Result<()>,
{
    fn rewrite(self, buf: &mut ByteCursor, writer: &mut W, cx: &mut C) -> anyhow::Result<()> {
        self(buf, writer, cx)
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum ScalarRewriteKind {
    VarU32,
    VarI32,
    U32,
    I32,
}

impl ScalarRewriteKind {
    pub fn read(self, buf: &mut ByteCursor) -> anyhow::Result<ScalarRewrite> {
        match self {
            Self::VarU32 => buf.read_var_u32().map(ScalarRewrite::VarU32),
            Self::VarI32 => buf.read_var_i32().map(ScalarRewrite::VarI32),
            Self::U32 => buf.read_u32().map(ScalarRewrite::U32),
            Self::I32 => buf.read_i32().map(ScalarRewrite::I32),
        }
    }

    pub fn with_value(self, value: u32) -> ScalarRewrite {
        match self {
            ScalarRewriteKind::VarU32 => ScalarRewrite::VarU32(value),
            ScalarRewriteKind::VarI32 => ScalarRewrite::VarI32(value as i32),
            ScalarRewriteKind::U32 => ScalarRewrite::U32(value),
            ScalarRewriteKind::I32 => ScalarRewrite::I32(value as i32),
        }
    }

    pub fn with_zeroed(self) -> ScalarRewrite {
        self.with_value(0)
    }
}

#[derive(Debug, Copy, Clone)]
pub enum ScalarRewrite {
    VarU32(u32),
    VarI32(i32),
    U32(u32),
    I32(i32),
}

impl ScalarRewrite {
    pub fn as_u32(self) -> u32 {
        use ScalarRewrite::*;

        match self {
            VarU32(v) => v,
            VarI32(v) => v as u32,
            U32(v) => v,
            I32(v) => v as u32,
        }
    }

    pub fn as_u32_neg_offset(self, addend: i32) -> u32 {
        self.as_u32().wrapping_add_signed(addend.wrapping_neg())
    }

    pub fn kind(self) -> ScalarRewriteKind {
        match self {
            ScalarRewrite::VarU32(_) => ScalarRewriteKind::VarU32,
            ScalarRewrite::VarI32(_) => ScalarRewriteKind::VarI32,
            ScalarRewrite::U32(_) => ScalarRewriteKind::U32,
            ScalarRewrite::I32(_) => ScalarRewriteKind::I32,
        }
    }

    pub fn rewrite_var_u32(
        buf: &mut ByteCursor,
        writer: &mut impl BufWriter,
        val: u32,
    ) -> anyhow::Result<()> {
        // N.B. relocation values are always full width in the LLVM spec.
        buf.read_var_u32_full()?;
        writer.write_var_u32_full(val);
        Ok(())
    }

    pub fn rewrite_var_i32(
        buf: &mut ByteCursor,
        writer: &mut impl BufWriter,
        val: i32,
    ) -> anyhow::Result<()> {
        // N.B. relocation values are always full width in the LLVM spec.
        buf.read_var_i32_full()?;
        writer.write_var_i32_full(val);
        Ok(())
    }

    pub fn rewrite_u32(
        buf: &mut ByteCursor,
        writer: &mut impl BufWriter,
        val: u32,
    ) -> anyhow::Result<()> {
        buf.read_u32()?;
        writer.write_u32(val);
        Ok(())
    }

    pub fn rewrite_i32(
        buf: &mut ByteCursor,
        writer: &mut impl BufWriter,
        val: i32,
    ) -> anyhow::Result<()> {
        buf.read_i32()?;
        writer.write_i32(val);
        Ok(())
    }
}

impl<W: BufWriter, C> Rewriter<W, C> for ScalarRewrite {
    fn rewrite(self, buf: &mut ByteCursor, writer: &mut W, _cx: &mut C) -> anyhow::Result<()> {
        use ScalarRewrite::*;

        match self {
            VarU32(val) => Self::rewrite_var_u32(buf, writer, val),
            VarI32(val) => Self::rewrite_var_i32(buf, writer, val),
            U32(val) => Self::rewrite_u32(buf, writer, val),
            I32(val) => Self::rewrite_i32(buf, writer, val),
        }
    }
}
