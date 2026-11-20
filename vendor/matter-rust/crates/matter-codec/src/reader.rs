//! Streaming TLV decoder.
//!
//! [`TlvReader::next`] walks the input one element at a time. Scalars are
//! returned as [`Element::Scalar`]; containers emit a [`Element::ContainerStart`]
//! immediately followed by the children's elements and then a matching
//! [`Element::ContainerEnd`]. Use [`TlvReader::read_value`] to materialise an
//! entire element tree in one call. [`TlvReader::next_ref`] is the zero-copy
//! sibling of [`TlvReader::next`]: it borrows string/bytes payloads straight
//! from the input instead of allocating, and is the decode core `next` itself
//! is implemented over.

use crate::error::{Error, Result};
use crate::tag::Tag;
use crate::value::{Value, ValueRef};
use crate::{element_type as et, tag_control as tc};

/// Which kind of TLV container a [`ContainerStart`](Element::ContainerStart)
/// announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerKind {
    /// `Value::Structure` — children carry their own (typically context-tagged) tags.
    Structure,
    /// `Value::Array` — children carry anonymous tags only.
    Array,
    /// `Value::List` — children may carry any tag form.
    List,
}

/// One step of the streaming reader.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Element {
    /// A complete scalar (or string/bytes) element.
    Scalar {
        /// The tag that identifies this element within its enclosing context.
        tag: Tag,
        /// The decoded scalar value.
        value: Value,
    },

    /// A container has just been opened. Subsequent `next()` calls
    /// return the container's children until a matching
    /// [`ContainerEnd`](Element::ContainerEnd).
    ContainerStart {
        /// The tag that identifies this container within its enclosing context.
        tag: Tag,
        /// Which container kind was opened.
        kind: ContainerKind,
    },

    /// The most recently-opened container has been closed.
    ContainerEnd,
}

/// One step of the streaming reader, borrowing string/bytes payloads from
/// the input — the zero-copy sibling of [`Element`]. Produced by
/// [`TlvReader::next_ref`]; convert with `Element::from` when ownership is
/// needed.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum ElementRef<'a> {
    /// A complete scalar (or string/bytes) element.
    Scalar {
        /// The tag that identifies this element within its enclosing context.
        tag: Tag,
        /// The decoded value, borrowing from the reader's input.
        value: ValueRef<'a>,
    },
    /// A container has just been opened (see [`Element::ContainerStart`]).
    ContainerStart {
        /// The tag that identifies this container within its enclosing context.
        tag: Tag,
        /// Which container kind was opened.
        kind: ContainerKind,
    },
    /// The most recently-opened container has been closed.
    ContainerEnd,
}

impl From<ElementRef<'_>> for Element {
    #[inline]
    fn from(e: ElementRef<'_>) -> Self {
        match e {
            ElementRef::Scalar { tag, value } => Element::Scalar {
                tag,
                value: Value::from(value),
            },
            ElementRef::ContainerStart { tag, kind } => Element::ContainerStart { tag, kind },
            ElementRef::ContainerEnd => Element::ContainerEnd,
        }
    }
}

/// Maximum container nesting depth the reader accepts. The Matter spec
/// recommends a 32-level limit to prevent stack blow-up on adversarial
/// input.
pub const MAX_DEPTH: usize = 32;

/// Default ceiling on the total number of [`Value`] elements a single
/// [`TlvReader::read_value`] call may materialise.
///
/// Tree-builder decoding allocates one `Value` (and, inside containers, one
/// `(Tag, Value)` pair) per element. A tiny scalar such as a boolean costs a
/// single wire byte but expands to a heap-resident `Value`, so an input packed
/// with millions of one-byte scalars can amplify into a large allocation. This
/// budget bounds that amplification: a decode that would produce more than this
/// many elements fails with [`Error::ElementBudgetExceeded`].
///
/// The default is deliberately generous (1,048,576 elements) — far above any
/// legitimate Matter payload, which is itself bounded by the protocol's
/// message-size limits — so it only ever trips on adversarial input. Callers
/// that need a tighter or looser bound can set their own via
/// [`TlvReader::with_element_budget`].
pub const DEFAULT_ELEMENT_BUDGET: usize = 1 << 20;

/// Byte offsets of one element within a [`TlvReader`]'s input, produced by
/// [`TlvReader::element_span`] / [`TlvReader::skip_container_span`].
///
/// Two-form contract:
/// - **Scalar span** — after `next()`/`next_ref()` returns a `Scalar`, the
///   span covers the complete element; [`body`](Self::body) starts after the
///   tag bytes (for strings it includes the length field, whose width form
///   lives in the element type).
/// - **Container span** — after `next()` returns `ContainerStart` the span
///   covers only the container header; call
///   [`skip_container_span`](TlvReader::skip_container_span) for the full
///   container, whose [`body`](Self::body) covers the children plus the
///   end-of-container marker and EXCLUDES the original control/tag bytes.
/// - **`ContainerEnd` span** — after `next()` returns `ContainerEnd`, the span
///   covers the 1-byte end-of-container marker and [`body`](Self::body) is empty.
///
/// Resolve ranges against the input with [`TlvReader::span_bytes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementSpan {
    start: usize,
    body_start: usize,
    end: usize,
}

impl ElementSpan {
    /// The complete element: control octet through the last body byte.
    #[must_use]
    pub fn full(&self) -> core::ops::Range<usize> {
        self.start..self.end
    }

    /// The element body: everything after the control octet and tag bytes.
    /// For a container span from `skip_container_span`, this is the children
    /// plus the end-of-container marker — the exact bytes retag re-emission
    /// copies under a fresh header.
    #[must_use]
    pub fn body(&self) -> core::ops::Range<usize> {
        self.body_start..self.end
    }
}

/// A streaming TLV decoder over a borrowed byte slice.
pub struct TlvReader<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
    /// Remaining element budget for a tree-builder decode. Decremented once
    /// per materialised [`Value`] in [`Self::read_container_body`] /
    /// [`Self::read_value`] — but only on *charged* decodes, i.e. when the
    /// remaining input is larger than this budget; a smaller input provably
    /// cannot exceed it (each element consumes ≥ 1 input byte), so the fast
    /// path skips accounting without weakening the bound (see
    /// [`Self::read_value`]). The streaming [`Self::next`] path does not
    /// touch it because it allocates nothing per element.
    element_budget: usize,
    /// Span of the element most recently returned by [`Self::next`] /
    /// [`Self::next_ref`] (or the whole container after a `skip_container*`
    /// call). `None` before the first element.
    last_span: Option<ElementSpan>,
    /// Whether the element most recently returned by [`Self::next_ref`] (and
    /// therefore [`Self::next`]) was a `ContainerStart` — the precondition
    /// [`Self::skip_container_span`] enforces so that a span is only ever
    /// handed out for a container the caller actually just opened. Set/cleared
    /// only on a returned element; untouched by `Ok(None)` and by errors, in
    /// lockstep with [`Self::last_span`].
    last_was_container_start: bool,
}

impl<'a> TlvReader<'a> {
    /// Construct a reader that walks `bytes` from the start, using the
    /// [`DEFAULT_ELEMENT_BUDGET`] for tree-builder decodes.
    #[inline]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            depth: 0,
            element_budget: DEFAULT_ELEMENT_BUDGET,
            last_span: None,
            last_was_container_start: false,
        }
    }

    /// Construct a reader with a custom total-element budget for tree-builder
    /// decoding (see [`DEFAULT_ELEMENT_BUDGET`]).
    ///
    /// A [`Self::read_value`] call that would materialise more than `budget`
    /// [`Value`] elements fails with [`Error::ElementBudgetExceeded`]. The
    /// budget only affects the tree-builder path; the streaming [`Self::next`]
    /// API is unaffected because it allocates nothing per element.
    #[inline]
    pub fn with_element_budget(bytes: &'a [u8], budget: usize) -> Self {
        Self {
            bytes,
            pos: 0,
            depth: 0,
            element_budget: budget,
            last_span: None,
            last_was_container_start: false,
        }
    }

    /// Whether there is no more input to consume.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    /// Advance one TLV element, returning an [`Element`] that owns its
    /// string/bytes payloads. Implemented over [`Self::next_ref`] — the
    /// borrowed walk IS the decode core, so the two can never disagree.
    /// Returns `Ok(None)` at end of input. See [`Self::next_ref`] for the
    /// zero-copy variant, which borrows string/bytes payloads from the
    /// input instead of allocating.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is malformed:
    ///
    /// - [`Error::InvalidTagControl`] — unrecognised tag-control byte form.
    /// - [`Error::InvalidElementType`] — unknown element-type code.
    /// - [`Error::UnexpectedEof`] — truncated payload bytes.
    /// - [`Error::UnexpectedEndOfContainer`] — end-of-container marker (`0x18`)
    ///   at the top level, with no container open.
    /// - [`Error::ContainerTooDeep`] — a container open would exceed
    ///   [`MAX_DEPTH`] nesting levels.
    ///
    /// # Note on naming
    ///
    /// This method is deliberately named `next` to match the streaming-reader
    /// idiom established by e.g. `serde`'s `Deserializer`. It returns
    /// `Result<Option<T>>` rather than `Option<Result<T>>` so that callers
    /// use `?` naturally. Implementing `std::iter::Iterator` is deferred to
    /// a later phase when a fallible-iterator adapter is available.
    #[allow(clippy::should_implement_trait)] // See note above; Iterator requires Option<Item>, not Result<Option<Item>>.
    #[inline]
    pub fn next(&mut self) -> Result<Option<Element>> {
        Ok(self.next_ref()?.map(Element::from))
    }

    /// Advance one TLV element, returning an [`ElementRef`] whose
    /// string/bytes payloads borrow directly from the reader's input — the
    /// zero-copy sibling of [`Self::next`], and the single decode core both
    /// methods share. Returns `Ok(None)` at end of input.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the input is malformed:
    ///
    /// - [`Error::InvalidTagControl`] — unrecognised tag-control byte form.
    /// - [`Error::InvalidElementType`] — unknown element-type code.
    /// - [`Error::UnexpectedEof`] — truncated payload bytes.
    /// - [`Error::UnexpectedEndOfContainer`] — end-of-container marker (`0x18`)
    ///   at the top level, with no container open.
    /// - [`Error::ContainerTooDeep`] — a container open would exceed
    ///   [`MAX_DEPTH`] nesting levels.
    #[inline]
    pub fn next_ref(&mut self) -> Result<Option<ElementRef<'a>>> {
        if self.is_empty() {
            return Ok(None);
        }
        let start = self.pos;
        let control = self.next_byte()?;
        let elem_type = control & et::ELEMENT_TYPE_MASK;

        // End-of-container is always emitted as anonymous tag form.
        if elem_type == et::END_OF_CONTAINER {
            if control & tc::TAG_CONTROL_MASK != tc::ANONYMOUS {
                return Err(Error::InvalidTagControl(control & tc::TAG_CONTROL_MASK));
            }
            if self.depth == 0 {
                return Err(Error::UnexpectedEndOfContainer);
            }
            self.depth -= 1;
            self.last_span = Some(ElementSpan {
                start,
                body_start: self.pos,
                end: self.pos,
            });
            self.last_was_container_start = false;
            return Ok(Some(ElementRef::ContainerEnd));
        }

        let tag = self.read_tag(control)?;
        let body_start = self.pos;

        // Container opens — record the kind and bump depth.
        let kind = match elem_type {
            et::STRUCTURE => Some(ContainerKind::Structure),
            et::ARRAY => Some(ContainerKind::Array),
            et::LIST => Some(ContainerKind::List),
            _ => None,
        };
        if let Some(kind) = kind {
            if self.depth >= MAX_DEPTH {
                return Err(Error::ContainerTooDeep);
            }
            self.depth += 1;
            self.last_span = Some(ElementSpan {
                start,
                body_start,
                end: self.pos,
            });
            self.last_was_container_start = true;
            return Ok(Some(ElementRef::ContainerStart { tag, kind }));
        }

        let value = self.read_value_body_ref(elem_type)?;
        self.last_span = Some(ElementSpan {
            start,
            body_start,
            end: self.pos,
        });
        self.last_was_container_start = false;
        Ok(Some(ElementRef::Scalar { tag, value }))
    }

    /// Skip the remaining body of the container whose
    /// [`ContainerStart`](Element::ContainerStart) was just returned by
    /// [`Self::next`], consuming through its matching
    /// [`ContainerEnd`](Element::ContainerEnd).
    ///
    /// Call this immediately after `next()` yields a `ContainerStart` you
    /// want to discard — for example an unknown field carried by a struct
    /// from a newer Matter revision. On return the reader is positioned at
    /// the first element *after* the skipped container. The container body
    /// is skipped as a raw byte walk: nothing is materialised, and string
    /// payloads inside the skipped (unobserved) region are **not** UTF-8
    /// validated. Cost is bounded by the input size and nesting by
    /// [`MAX_DEPTH`].
    ///
    /// # Errors
    ///
    /// - [`Error::UnclosedContainer`] — end of input before the container's
    ///   closing marker.
    /// - [`Error::InvalidTagControl`] — unrecognised tag-control byte form.
    /// - [`Error::InvalidElementType`] — unknown element-type code.
    /// - [`Error::UnexpectedEof`] — truncated payload bytes or a truncated
    ///   length field.
    /// - [`Error::ContainerTooDeep`] — a container open inside the skipped
    ///   region would exceed [`MAX_DEPTH`] nesting levels.
    /// - [`Error::UnexpectedEndOfContainer`] — called with no container open
    ///   on this reader.
    ///
    /// After a successful skip, [`Self::element_span`] reports the whole
    /// skipped container (header through end-of-container marker) — same as
    /// calling [`Self::skip_container_span`] and discarding the return value.
    ///
    /// # Examples
    ///
    /// ```
    /// use matter_codec::{ContainerKind, Element, Tag, TlvReader, TlvWriter};
    /// let mut buf = Vec::new();
    /// let mut w = TlvWriter::new(&mut buf);
    /// w.start_structure(Tag::Anonymous)?;
    /// w.start_structure(Tag::Context(9))?; // an unknown nested field
    /// w.end_container()?;
    /// w.put_uint(Tag::Context(1), 42)?;
    /// w.end_container()?;
    ///
    /// let mut r = TlvReader::new(&buf);
    /// r.next()?; // open the outer struct
    /// // next() returns the nested ctx9 ContainerStart we want to discard:
    /// assert!(matches!(
    ///     r.next()?,
    ///     Some(Element::ContainerStart { kind: ContainerKind::Structure, .. })
    /// ));
    /// r.skip_container()?; // drain the nested struct
    /// // the field after the unknown container is still readable:
    /// assert!(matches!(r.next()?, Some(Element::Scalar { tag: Tag::Context(1), .. })));
    /// # Ok::<(), matter_codec::Error>(())
    /// ```
    ///
    /// After an `Err` from this method, the reader's position and depth are
    /// unspecified — discard the reader rather than continuing to iterate.
    pub fn skip_container(&mut self) -> Result<()> {
        self.skip_container_body()?;
        if let Some(h) = self.last_span {
            self.last_span = Some(ElementSpan {
                start: h.start,
                body_start: h.body_start,
                end: self.pos,
            });
        }
        // The container that was open has been consumed: a following
        // `skip_container_span` must not treat this as a fresh ContainerStart.
        self.last_was_container_start = false;
        Ok(())
    }

    /// Like [`Self::skip_container`], but returns the skipped container's
    /// [`ElementSpan`] (marked at the `ContainerStart` just returned by
    /// `next()`; ended at the read position after the raw skip).
    ///
    /// This method has a precondition: the immediately preceding
    /// [`Self::next`] / [`Self::next_ref`] call must have returned a
    /// `ContainerStart`. Anywhere else — after a scalar, after an
    /// end-of-container, after a [`Self::read_value`] that walked a whole
    /// tree, after another skip, or before any element at all — it consumes
    /// nothing and returns [`Error::UnexpectedEndOfContainer`], rather than
    /// handing back a span over unrelated bytes that a retag caller would
    /// re-emit as malformed TLV.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEndOfContainer`] if the precondition above does not
    /// hold, plus every error [`Self::skip_container`] can return. Misuse is
    /// always an error — never a panic, and never a meaningless span.
    ///
    /// After an `Err` from this method, the reader's position and depth are
    /// unspecified — discard the reader rather than continuing to iterate.
    pub fn skip_container_span(&mut self) -> Result<ElementSpan> {
        if !self.last_was_container_start {
            return Err(Error::UnexpectedEndOfContainer);
        }
        let header = self.last_span.ok_or(Error::UnexpectedEndOfContainer)?;
        self.skip_container_body()?;
        let span = ElementSpan {
            start: header.start,
            body_start: header.body_start,
            end: self.pos,
        };
        self.last_span = Some(span);
        self.last_was_container_start = false;
        Ok(span)
    }

    /// Finish a top-level decode: close every container still open, then
    /// require that no input remains.
    ///
    /// A Matter message (and a command-fields blob) is exactly one TLV
    /// element, normally an anonymous structure. Call this once the parser
    /// has what it needs. Any members the caller did not read, nested
    /// containers included, are skipped through the top-level
    /// end-of-container, and then the input must be exhausted. This is the
    /// same check chip's `MessageParser::ExitContainer` makes on every
    /// Interaction Model message (`src/app/MessageDef/MessageParser.cpp`):
    /// `TLVReader::ExitContainer` fails if end of input comes before the
    /// container's end, and a following `Next()` must return
    /// `CHIP_END_OF_TLV`.
    ///
    /// Without it, a parser that loops until `next()` returns `Ok(None)`
    /// treats end of input as the structure's close, so a truncated message
    /// decodes as if it were complete, and bytes after the close are ignored.
    ///
    /// Skipped members get the structural validation of
    /// [`Self::skip_container`] (tag forms, element types, bounds, nesting
    /// depth) but are not materialised, and their strings are not UTF-8
    /// validated. Calling `finish` with no container open checks only that
    /// the input is exhausted: it succeeds on an empty reader and on one that
    /// has read a single complete scalar, and fails on a fresh reader over
    /// non-empty input. [`Self::element_span`] is not updated.
    ///
    /// The skipped members are subject to the [`MAX_DEPTH`] nesting limit, so
    /// a subtree nested deeper than that fails with
    /// [`Error::ContainerTooDeep`], even in a part of the message the caller
    /// never reads. chip's `TLVReader::SkipToEndOfContainer` has no depth
    /// bound, so it would accept such a message. No Interaction Model message
    /// comes near that depth.
    ///
    /// # Errors
    ///
    /// - [`Error::UnclosedContainer`] — end of input before the end-of-container
    ///   of an open container (the message was truncated).
    /// - [`Error::TrailingData`] — input remains after the top-level element.
    /// - [`Error::UnexpectedEof`], [`Error::InvalidTagControl`],
    ///   [`Error::InvalidElementType`], [`Error::ContainerTooDeep`] — the
    ///   unread remainder is malformed, as for [`Self::skip_container`].
    ///
    /// After an `Err`, the reader's position and depth are unspecified —
    /// discard the reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use matter_codec::{Error, TlvReader};
    /// // { ctx0: 1 } — read only the struct start, let finish do the rest.
    /// let mut r = TlvReader::new(&[0x15, 0x24, 0x00, 0x01, 0x18]);
    /// r.next()?;
    /// r.finish()?;
    ///
    /// // Truncated: the end-of-container is missing.
    /// let mut r = TlvReader::new(&[0x15, 0x24, 0x00, 0x01]);
    /// r.next()?;
    /// assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
    ///
    /// // A byte after the end-of-container.
    /// let mut r = TlvReader::new(&[0x15, 0x18, 0x00]);
    /// r.next()?;
    /// assert!(matches!(r.finish(), Err(Error::TrailingData)));
    /// # Ok::<(), matter_codec::Error>(())
    /// ```
    pub fn finish(&mut self) -> Result<()> {
        // Each successful skip consumes one open container through its
        // end-of-container and decrements `self.depth` by exactly one, so
        // this loop runs at most `MAX_DEPTH` times. A truncated body fails
        // inside the skip with `UnclosedContainer`.
        while self.depth > 0 {
            self.skip_container_body()?;
            self.last_was_container_start = false;
        }
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingData)
        }
    }

    /// Span of the element most recently returned by [`Self::next`] /
    /// [`Self::next_ref`] (or the whole container after a
    /// `skip_container*` call). `None` before the first element; unchanged
    /// by calls that return `Ok(None)` or an error.
    ///
    /// Tree-builder reads drive the same core: [`Self::read_value`] walks its
    /// element via [`Self::next_ref`] internally, so afterwards the span
    /// refers to the last *interior* element it consumed, not the tree as a
    /// whole. Read the span only immediately after the `next` / `next_ref`
    /// call whose element you care about.
    #[inline]
    #[must_use]
    pub fn element_span(&self) -> Option<ElementSpan> {
        self.last_span
    }

    /// Resolve a range produced by this reader's span APIs against the
    /// reader's input. Returns an empty slice for a range that does not lie
    /// within the input (only possible with a span from a different reader).
    #[inline]
    #[must_use]
    pub fn span_bytes(&self, range: core::ops::Range<usize>) -> &'a [u8] {
        self.bytes.get(range).unwrap_or(&[])
    }

    /// Raw byte walk: control byte → tag skip → body skip. Nothing is
    /// materialised and string payloads are advanced over without UTF-8
    /// validation (deliberate: skipped data is unobserved, and validating
    /// it cost time on every skip). Structural validation (tag forms,
    /// element types, bounds, nesting depth) is identical to `next()`.
    fn skip_container_body(&mut self) -> Result<()> {
        let mut depth = 1usize;
        while depth > 0 {
            if self.is_empty() {
                return Err(Error::UnclosedContainer);
            }
            let control = self.next_byte()?;
            let elem_type = control & et::ELEMENT_TYPE_MASK;
            if elem_type == et::END_OF_CONTAINER {
                if control & tc::TAG_CONTROL_MASK != tc::ANONYMOUS {
                    return Err(Error::InvalidTagControl(control & tc::TAG_CONTROL_MASK));
                }
                if depth == 1 && self.depth == 0 {
                    // No container is actually open on this reader — the
                    // same misuse `next()` reports for a stray end marker.
                    return Err(Error::UnexpectedEndOfContainer);
                }
                depth -= 1;
                continue;
            }
            // Parse-and-discard the tag: allocation-free, and keeps tag-form
            // validation identical to `next()`.
            let _ = self.read_tag(control)?;
            match elem_type {
                et::STRUCTURE | et::ARRAY | et::LIST => {
                    // `next()` would error when opening a child at
                    // self.depth >= MAX_DEPTH; during a skip that effective
                    // depth is reader depth + local nesting - 1, so this is
                    // `self.depth + depth - 1 >= MAX_DEPTH` rearranged to
                    // avoid clippy::int_plus_one.
                    if self.depth + depth > MAX_DEPTH {
                        return Err(Error::ContainerTooDeep);
                    }
                    depth += 1;
                }
                _ => self.skip_value_body(elem_type)?,
            }
        }
        // The skipped container's ContainerStart incremented self.depth in
        // next(); its end marker was consumed here rather than via next(),
        // so balance the counter. Cannot underflow: reaching this point
        // required consuming an end marker at local depth 1, which the
        // misuse guard above rejects when self.depth == 0.
        self.depth -= 1;
        Ok(())
    }

    /// Advance past a scalar body during a raw skip, without materialising
    /// it. String/bytes payloads are bounds-checked and skipped whole; no
    /// UTF-8 validation is performed on skipped data.
    fn skip_value_body(&mut self, elem_type: u8) -> Result<()> {
        let n = match elem_type {
            et::BOOL_FALSE | et::BOOL_TRUE | et::NULL => 0,
            et::UINT8 | et::INT8 => 1,
            et::UINT16 | et::INT16 => 2,
            et::UINT32 | et::INT32 | et::FLOAT32 => 4,
            et::UINT64 | et::INT64 | et::FLOAT64 => 8,
            et::UTF8_LEN8
            | et::UTF8_LEN16
            | et::UTF8_LEN32
            | et::UTF8_LEN64
            | et::BYTES_LEN8
            | et::BYTES_LEN16
            | et::BYTES_LEN32
            | et::BYTES_LEN64 => {
                let len = self.read_payload_len(elem_type)?;
                let _ = self.next_bytes(len)?;
                return Ok(());
            }
            other => return Err(Error::InvalidElementType(other)),
        };
        let _ = self.next_bytes(n)?;
        Ok(())
    }

    /// Materialise one full TLV element as a `(Tag, Value)`. Scalars are
    /// returned directly; containers are read recursively up to
    /// [`MAX_DEPTH`] levels (enforced by [`Self::next`]'s depth counter).
    ///
    /// # Errors
    ///
    /// - [`Error::UnexpectedEof`] — the input is empty.
    /// - [`Error::UnexpectedEndOfContainer`] — the first element is a stray
    ///   end-of-container marker.
    /// - [`Error::UnclosedContainer`] — end of input was reached before the
    ///   container's closing marker.
    /// - [`Error::NonAnonymousArrayTag`] — an array child carried a
    ///   non-anonymous tag (the spec requires array elements to be anonymous).
    /// - [`Error::ElementBudgetExceeded`] — the decode would materialise more
    ///   than the configured element budget (see [`DEFAULT_ELEMENT_BUDGET`]).
    /// - Any error returned by [`Self::next`].
    pub fn read_value(&mut self) -> Result<(Tag, Value)> {
        // Budget fast path: every materialised element consumes at least one
        // input byte (a null is one control byte; a container is a control
        // byte plus its end marker), so when the remaining input is no larger
        // than the remaining budget this decode CANNOT exceed it — decode
        // with per-element accounting compiled out entirely. Real Matter
        // payloads (≲ 1.5 KiB) against the 2^20 default budget always take
        // this path; the charged path serves oversized or custom-budget
        // inputs.
        //
        // This is observably equivalent to charging every element: a call is
        // only admitted uncharged when the byte bound proves it cannot fail,
        // and from such a call onward every future element from this reader
        // is covered by the same bound (the remaining input only shrinks), so
        // sequences of `read_value` calls fail on exactly the same element —
        // with the same error — as the always-charged implementation. The
        // lifetime invariant is unchanged: one reader never materialises more
        // than its configured budget's worth of live `Value` elements.
        let remaining_input = self.bytes.len().saturating_sub(self.pos);
        if remaining_input <= self.element_budget {
            self.read_value_inner::<false>()
        } else {
            self.read_value_inner::<true>()
        }
    }

    /// Tree-builder body of [`Self::read_value`], monomorphised over whether
    /// per-element budget accounting is active (see the fast-path comment
    /// there — `CHARGE == false` is only reachable when the byte bound proves
    /// the budget cannot be exceeded).
    ///
    /// Drives [`Self::next_ref`] rather than [`Self::next`]: the owned
    /// [`Element`] `next()` would build is an intermediate this path
    /// immediately destructures again, and materialising it per element cost
    /// ~2.5x on the decode benchmarks (2026-08-10 phase-4 regression). The
    /// borrowed element is destructured here and only its payload is taken
    /// into ownership, at the point where it is pushed into the tree.
    fn read_value_inner<const CHARGE: bool>(&mut self) -> Result<(Tag, Value)> {
        match self.next_ref()? {
            Some(ElementRef::Scalar { tag, value }) => {
                if CHARGE {
                    self.charge_element()?;
                }
                Ok((tag, Value::from(value)))
            }
            Some(ElementRef::ContainerStart { tag, kind }) => {
                if CHARGE {
                    self.charge_element()?;
                }
                let value = self.read_container_body::<CHARGE>(kind)?;
                Ok((tag, value))
            }
            Some(ElementRef::ContainerEnd) => Err(Error::UnexpectedEndOfContainer),
            None => Err(Error::UnexpectedEof),
        }
    }

    /// Charge one element against the tree-builder budget. Returns
    /// [`Error::ElementBudgetExceeded`] once the budget is exhausted. Only
    /// called on charged decodes ([`Self::read_value`]'s slow path); the
    /// container loops mirror the same arithmetic in a local for speed (see
    /// [`Self::read_container_body`]).
    fn charge_element(&mut self) -> Result<()> {
        self.element_budget = self
            .element_budget
            .checked_sub(1)
            .ok_or(Error::ElementBudgetExceeded)?;
        Ok(())
    }

    /// Decode a container's children into a [`Value`] tree.
    ///
    /// # Element-budget accounting (denial-of-service bound)
    ///
    /// When `CHARGE` is false (the [`Self::read_value`] fast path: the byte
    /// bound already proves the budget cannot be exceeded) no accounting code
    /// is compiled into this copy at all.
    ///
    /// When `CHARGE` is true, the check is the hot path of tree-builder
    /// decoding, so instead of calling [`Self::charge_element`] (a
    /// read-modify-write of `self.element_budget` that the compiler cannot
    /// keep in a register across the `self.next()` calls) each loop mirrors
    /// the remaining budget in a local, charges the local per element, and
    /// syncs it back to `self.element_budget` around recursion and at
    /// container close.
    ///
    /// The preserved invariant: **a charged decode never materialises more
    /// than the configured element budget's worth of [`Value`] elements, and
    /// it fails (`Error::ElementBudgetExceeded`) on exactly the same element
    /// as a per-element field update would** — every element is still
    /// individually charged before it is pushed, and the field is up to date
    /// whenever recursion (the only other consumer) runs. The one observable
    /// difference is on *error* returns: charges made since the last sync are
    /// not written back — which cannot weaken the bound, because the failed
    /// call's partially built tree is dropped with it (the budget bounds live
    /// memory amplification, and a subsequent `read_value` on the same reader
    /// starts from a tree of zero live elements).
    fn read_container_body<const CHARGE: bool>(&mut self, kind: ContainerKind) -> Result<Value> {
        // Both loops walk `next_ref` and convert the borrowed payload at push
        // time — see `read_value_inner` for why the owned `Element` step is
        // skipped here.
        //
        // Branch on the container kind once, before the loop, so arrays decode
        // straight into a `Vec<Value>` without first building a `Vec<(Tag,
        // Value)>` and re-collecting. Structures and lists keep their members'
        // tags.
        match kind {
            ContainerKind::Array => {
                let mut elements: Vec<Value> = Vec::new();
                let mut budget = self.element_budget;
                loop {
                    match self.next_ref()? {
                        None => return Err(Error::UnclosedContainer),
                        Some(ElementRef::ContainerEnd) => break,
                        Some(ElementRef::Scalar { tag, value }) => {
                            // Spec: every array element must be anonymous. Fail
                            // closed on any other tag rather than discarding it.
                            if tag != Tag::Anonymous {
                                return Err(Error::NonAnonymousArrayTag);
                            }
                            if CHARGE {
                                budget =
                                    budget.checked_sub(1).ok_or(Error::ElementBudgetExceeded)?;
                            }
                            elements.push(Value::from(value));
                        }
                        Some(ElementRef::ContainerStart {
                            tag,
                            kind: inner_kind,
                        }) => {
                            if tag != Tag::Anonymous {
                                return Err(Error::NonAnonymousArrayTag);
                            }
                            if CHARGE {
                                budget =
                                    budget.checked_sub(1).ok_or(Error::ElementBudgetExceeded)?;
                                self.element_budget = budget;
                            }
                            elements.push(self.read_container_body::<CHARGE>(inner_kind)?);
                            if CHARGE {
                                budget = self.element_budget;
                            }
                        }
                    }
                }
                if CHARGE {
                    self.element_budget = budget;
                }
                Ok(Value::Array(elements))
            }
            ContainerKind::Structure | ContainerKind::List => {
                let mut members: Vec<(Tag, Value)> = Vec::new();
                let mut budget = self.element_budget;
                loop {
                    match self.next_ref()? {
                        None => return Err(Error::UnclosedContainer),
                        Some(ElementRef::ContainerEnd) => break,
                        Some(ElementRef::Scalar { tag, value }) => {
                            if CHARGE {
                                budget =
                                    budget.checked_sub(1).ok_or(Error::ElementBudgetExceeded)?;
                            }
                            members.push((tag, Value::from(value)));
                        }
                        Some(ElementRef::ContainerStart {
                            tag,
                            kind: inner_kind,
                        }) => {
                            if CHARGE {
                                budget =
                                    budget.checked_sub(1).ok_or(Error::ElementBudgetExceeded)?;
                                self.element_budget = budget;
                            }
                            let inner = self.read_container_body::<CHARGE>(inner_kind)?;
                            members.push((tag, inner));
                            if CHARGE {
                                budget = self.element_budget;
                            }
                        }
                    }
                }
                if CHARGE {
                    self.element_budget = budget;
                }
                Ok(match kind {
                    ContainerKind::List => Value::List(members),
                    // The outer match guarantees this arm is Structure.
                    _ => Value::Structure(members),
                })
            }
        }
    }

    #[inline]
    fn next_byte(&mut self) -> Result<u8> {
        let b = *self.bytes.get(self.pos).ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        Ok(b)
    }

    #[inline]
    fn next_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::LengthOverflow)?;
        let slice = self.bytes.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(slice)
    }

    // The decode helpers below (`read_tag`, `read_value_body_ref`, the
    // fixed-width and length-field readers) are each one small step of a
    // single element's decode. Until phase 4 they lived inside one monolithic
    // `next()` and were inlined into it implicitly; once the core moved into
    // `next_ref` — itself `#[inline]`, so it is inlined into its callers and
    // then re-optimised there — LLVM stopped inlining them and every element
    // paid a call per step (measured: +73% on `decode/struct_500_uint`).
    // Marking them explicitly restores the pre-phase-4 shape.
    #[inline]
    fn read_tag(&mut self, control: u8) -> Result<Tag> {
        match control & tc::TAG_CONTROL_MASK {
            tc::ANONYMOUS => Ok(Tag::Anonymous),
            tc::CONTEXT => {
                let n = self.next_byte()?;
                Ok(Tag::Context(n))
            }
            tc::COMMON_PROFILE_2 => {
                let raw: [u8; 2] = self
                    .next_bytes(2)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(Tag::CommonProfile(u32::from(u16::from_le_bytes(raw))))
            }
            tc::COMMON_PROFILE_4 => {
                let raw: [u8; 4] = self
                    .next_bytes(4)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(Tag::CommonProfile(u32::from_le_bytes(raw)))
            }
            tc::IMPLICIT_PROFILE_2 => {
                let raw: [u8; 2] = self
                    .next_bytes(2)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(Tag::ImplicitProfile(u32::from(u16::from_le_bytes(raw))))
            }
            tc::IMPLICIT_PROFILE_4 => {
                let raw: [u8; 4] = self
                    .next_bytes(4)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(Tag::ImplicitProfile(u32::from_le_bytes(raw)))
            }
            tc::FULLY_QUALIFIED_6 => {
                let vendor = self.read_u16_le()?;
                let profile = self.read_u16_le()?;
                let tag = u32::from(self.read_u16_le()?);
                Ok(Tag::FullyQualified {
                    vendor,
                    profile,
                    tag,
                })
            }
            tc::FULLY_QUALIFIED_8 => {
                let vendor = self.read_u16_le()?;
                let profile = self.read_u16_le()?;
                let tag = self.read_u32_le()?;
                Ok(Tag::FullyQualified {
                    vendor,
                    profile,
                    tag,
                })
            }
            // The 3-bit tag-control field has only 8 possible values, and
            // we have arms for all 8. This arm is unreachable in practice
            // but rustc cannot prove that statically.
            other => Err(Error::InvalidTagControl(other)),
        }
    }

    #[inline]
    fn read_u16_le(&mut self) -> Result<u16> {
        let raw: [u8; 2] = self
            .next_bytes(2)?
            .try_into()
            .map_err(|_| Error::InternalSliceConversion)?;
        Ok(u16::from_le_bytes(raw))
    }

    #[inline]
    fn read_u32_le(&mut self) -> Result<u32> {
        let raw: [u8; 4] = self
            .next_bytes(4)?
            .try_into()
            .map_err(|_| Error::InternalSliceConversion)?;
        Ok(u32::from_le_bytes(raw))
    }

    #[allow(clippy::cast_possible_wrap)] // `b as i8`: reinterprets the byte pattern as signed, not truncation.
    #[inline]
    fn read_value_body_ref(&mut self, elem_type: u8) -> Result<ValueRef<'a>> {
        match elem_type {
            et::BOOL_FALSE => Ok(ValueRef::Bool(false)),
            et::BOOL_TRUE => Ok(ValueRef::Bool(true)),
            et::NULL => Ok(ValueRef::Null),
            et::UINT8 => Ok(ValueRef::Uint(u64::from(self.next_byte()?))),
            et::UINT16 => {
                let raw: [u8; 2] = self
                    .next_bytes(2)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Uint(u64::from(u16::from_le_bytes(raw))))
            }
            et::UINT32 => {
                let raw: [u8; 4] = self
                    .next_bytes(4)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Uint(u64::from(u32::from_le_bytes(raw))))
            }
            et::UINT64 => {
                let raw: [u8; 8] = self
                    .next_bytes(8)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Uint(u64::from_le_bytes(raw)))
            }
            et::INT8 => {
                let b = self.next_byte()?;
                Ok(ValueRef::Int(i64::from(b as i8)))
            }
            et::INT16 => {
                let raw: [u8; 2] = self
                    .next_bytes(2)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Int(i64::from(i16::from_le_bytes(raw))))
            }
            et::INT32 => {
                let raw: [u8; 4] = self
                    .next_bytes(4)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Int(i64::from(i32::from_le_bytes(raw))))
            }
            et::INT64 => {
                let raw: [u8; 8] = self
                    .next_bytes(8)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Int(i64::from_le_bytes(raw)))
            }
            et::FLOAT32 => {
                let raw: [u8; 4] = self
                    .next_bytes(4)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Float(f32::from_le_bytes(raw)))
            }
            et::FLOAT64 => {
                let raw: [u8; 8] = self
                    .next_bytes(8)?
                    .try_into()
                    .map_err(|_| Error::InternalSliceConversion)?;
                Ok(ValueRef::Double(f64::from_le_bytes(raw)))
            }
            et::UTF8_LEN8 | et::UTF8_LEN16 | et::UTF8_LEN32 | et::UTF8_LEN64 => {
                let len = self.read_payload_len(elem_type)?;
                self.read_utf8_ref(len)
            }
            et::BYTES_LEN8 | et::BYTES_LEN16 | et::BYTES_LEN32 | et::BYTES_LEN64 => {
                let len = self.read_payload_len(elem_type)?;
                self.read_bytes_ref(len)
            }
            other => Err(Error::InvalidElementType(other)),
        }
    }

    /// Read the variable-width length field that precedes utf8 and bytes
    /// payloads. The two low bits of the element type encode the width:
    /// `0b00` = 1 byte, `0b01` = 2 bytes, `0b10` = 4 bytes, `0b11` = 8 bytes.
    #[inline]
    fn read_payload_len(&mut self, elem_type: u8) -> Result<usize> {
        match elem_type & 0b11 {
            0b00 => Ok(usize::from(self.next_byte()?)),
            0b01 => Ok(usize::from(self.read_u16_le()?)),
            0b10 => usize::try_from(self.read_u32_le()?).map_err(|_| Error::LengthOverflow),
            _ => usize::try_from(self.read_u64_le()?).map_err(|_| Error::LengthOverflow),
        }
    }

    #[inline]
    fn read_u64_le(&mut self) -> Result<u64> {
        let raw: [u8; 8] = self
            .next_bytes(8)?
            .try_into()
            .map_err(|_| Error::InternalSliceConversion)?;
        Ok(u64::from_le_bytes(raw))
    }

    #[inline]
    fn read_utf8_ref(&mut self, len: usize) -> Result<ValueRef<'a>> {
        let bytes = self.next_bytes(len)?;
        // Validate UTF-8 across the ENTIRE payload (a malformed suffix still
        // fails), then present only the text before the first IS1 (0x1F)
        // separator. Matter uses IS1 to separate a char string's text from an
        // optional localized-string language suffix (`"Kitchen\u{1F}0409"`);
        // chip's `TLVReader::Get(CharSpan&)` and matter.js both return only the
        // text (chip TestTLV.cpp CheckTLVCharSpan). Returning the whole payload
        // surfaced the raw separator+suffix on reads of localized labels
        // (BasicInformation NodeLabel, UserLabel). The raw suffix
        // (LSID) is not yet exposed; that is a separate additive follow-up.
        let s = core::str::from_utf8(bytes)?;
        // IS1 (0x1F) is single-byte ASCII, so the split index is a valid char
        // boundary.
        let text = match s.find('\u{1F}') {
            Some(i) => &s[..i],
            None => s,
        };
        Ok(ValueRef::Utf8(text))
    }

    #[inline]
    fn read_bytes_ref(&mut self, len: usize) -> Result<ValueRef<'a>> {
        Ok(ValueRef::Bytes(self.next_bytes(len)?))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap is allowed in tests with a documented justification (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn next_returns_none_on_empty_input() {
        let mut r = TlvReader::new(&[]);
        assert!(r.is_empty());
        assert_eq!(r.next().unwrap(), None);
    }

    #[test]
    fn next_decodes_bool_true_anonymous_vector_0001() {
        let mut r = TlvReader::new(&[0x09]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Bool(true)
            }
        );
        assert!(r.is_empty());
    }

    #[test]
    fn next_decodes_bool_false() {
        let mut r = TlvReader::new(&[0x08]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Bool(false)
            }
        );
    }

    #[test]
    fn next_decodes_null_vector_implied() {
        let mut r = TlvReader::new(&[0x14]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Null
            }
        );
    }

    #[test]
    fn next_decodes_uint8_42_vector_0003() {
        let mut r = TlvReader::new(&[0x04, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_decodes_uint16_0x1234() {
        let mut r = TlvReader::new(&[0x05, 0x34, 0x12]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Uint(0x1234)
            }
        );
    }

    #[test]
    fn next_decodes_uint32_0xcafebabe() {
        let mut r = TlvReader::new(&[0x06, 0xBE, 0xBA, 0xFE, 0xCA]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Uint(0xCAFE_BABE)
            }
        );
    }

    #[test]
    fn next_decodes_uint64_big() {
        let bytes = [0x07, 0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Uint(0x0123_4567_89AB_CDEF),
            }
        );
    }

    #[test]
    fn next_decodes_int8_neg17_vector_0008() {
        let mut r = TlvReader::new(&[0x00, 0xEF]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Int(-17)
            }
        );
    }

    #[test]
    fn next_decodes_int16_neg129() {
        let mut r = TlvReader::new(&[0x01, 0x7F, 0xFF]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Int(-129)
            }
        );
    }

    #[test]
    fn next_decodes_int32_min() {
        let mut r = TlvReader::new(&[0x02, 0x00, 0x00, 0x00, 0x80]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Int(i64::from(i32::MIN))
            }
        );
    }

    #[test]
    fn next_decodes_int64_min() {
        let bytes = [0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Int(i64::MIN)
            }
        );
    }

    #[test]
    fn next_decodes_float32_zero_vector_0013() {
        let mut r = TlvReader::new(&[0x0A, 0x00, 0x00, 0x00, 0x00]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Float(0.0)
            }
        );
    }

    #[test]
    fn next_decodes_float64_zero_vector_0014() {
        let bytes = [0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Double(0.0)
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_context_tag_5() {
        let mut r = TlvReader::new(&[0x24, 0x05, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Context(5),
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_errors_on_unexpected_eof_in_payload() {
        let mut r = TlvReader::new(&[0x05]); // uint16 with no payload
        assert!(matches!(r.next(), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn next_decodes_uint_with_common_profile_2_byte_tag() {
        let mut r = TlvReader::new(&[0x44, 0x07, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::CommonProfile(7),
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_common_profile_4_byte_tag() {
        let mut r = TlvReader::new(&[0x64, 0x45, 0x23, 0x01, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::CommonProfile(0x0001_2345),
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_implicit_profile_2_byte_tag() {
        let mut r = TlvReader::new(&[0x84, 0x07, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::ImplicitProfile(7),
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_implicit_profile_4_byte_tag() {
        let mut r = TlvReader::new(&[0xA4, 0x45, 0x23, 0x01, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::ImplicitProfile(0x0001_2345),
                value: Value::Uint(42)
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_fully_qualified_6_byte() {
        let mut r = TlvReader::new(&[0xC4, 0xF1, 0xFF, 0x06, 0x00, 0x05, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::FullyQualified {
                    vendor: 0xFFF1,
                    profile: 0x0006,
                    tag: 5
                },
                value: Value::Uint(42),
            }
        );
    }

    #[test]
    fn next_decodes_uint_with_fully_qualified_8_byte() {
        let mut r = TlvReader::new(&[0xE4, 0xF1, 0xFF, 0x06, 0x00, 0x45, 0x23, 0x01, 0x00, 0x2A]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::FullyQualified {
                    vendor: 0xFFF1,
                    profile: 0x0006,
                    tag: 0x0001_2345
                },
                value: Value::Uint(42),
            }
        );
    }

    #[test]
    fn read_value_returns_tag_and_value_for_scalar() {
        let mut r = TlvReader::new(&[0x24, 0x05, 0x2A]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Context(5));
        assert_eq!(value, Value::Uint(42));
    }

    #[test]
    fn read_value_errors_on_empty_input() {
        let mut r = TlvReader::new(&[]);
        assert!(matches!(r.read_value(), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn next_decodes_utf8_hello_vector_0015() {
        let bytes = [0x0C, 0x06, 0x48, 0x65, 0x6C, 0x6C, 0x6F, 0x21];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Utf8(String::from("Hello!")),
            }
        );
    }

    #[test]
    fn next_decodes_utf8_empty_vector_0016() {
        let bytes = [0x0C, 0x00];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Utf8(String::new()),
            }
        );
    }

    #[test]
    fn next_decodes_utf8_len16_path() {
        let mut bytes = vec![0x0D, 0x00, 0x01]; // UTF8_LEN16, length 256 LE
        bytes.extend(std::iter::repeat_n(b'a', 256));
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        let Element::Scalar {
            value: Value::Utf8(s),
            ..
        } = el
        else {
            panic!("wrong variant")
        };
        assert_eq!(s.len(), 256);
        assert!(s.bytes().all(|b| b == b'a'));
    }

    #[test]
    fn next_decodes_bytes_five_bytes_vector_0017() {
        let bytes = [0x10, 0x05, 0x00, 0x01, 0x02, 0x03, 0x04];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Bytes(vec![0x00, 0x01, 0x02, 0x03, 0x04]),
            }
        );
    }

    #[test]
    fn next_decodes_bytes_empty_vector_0018() {
        let bytes = [0x10, 0x00];
        let mut r = TlvReader::new(&bytes);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Bytes(Vec::new()),
            }
        );
    }

    #[test]
    fn next_errors_on_invalid_utf8() {
        let bytes = [0x0C, 0x01, 0xFF];
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(r.next(), Err(Error::InvalidUtf8(_))));
    }

    #[test]
    fn next_errors_on_truncated_utf8_payload() {
        let bytes = [0x0C, 0x05, b'H', b'i']; // claims 5 bytes, has only 2
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(r.next(), Err(Error::UnexpectedEof)));
    }

    // --- container event tests ---

    #[test]
    fn next_decodes_structure_start_and_end_vector_0019() {
        let mut r = TlvReader::new(&[0x15, 0x18]);
        let el = r.next().unwrap().unwrap();
        assert_eq!(
            el,
            Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            }
        );
        let el = r.next().unwrap().unwrap();
        assert_eq!(el, Element::ContainerEnd);
        assert!(r.next().unwrap().is_none());
    }

    #[test]
    fn next_decodes_array_start_and_end_vector_0020() {
        let mut r = TlvReader::new(&[0x16, 0x18]);
        assert_eq!(
            r.next().unwrap().unwrap(),
            Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Array,
            }
        );
        assert_eq!(r.next().unwrap().unwrap(), Element::ContainerEnd);
    }

    #[test]
    fn next_decodes_list_start_and_end() {
        let mut r = TlvReader::new(&[0x17, 0x18]);
        assert_eq!(
            r.next().unwrap().unwrap(),
            Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::List,
            }
        );
        assert_eq!(r.next().unwrap().unwrap(), Element::ContainerEnd);
    }

    #[test]
    fn next_decodes_structure_with_child_streaming() {
        let mut r = TlvReader::new(&[0x15, 0x24, 0x00, 0x2A, 0x18]);
        assert_eq!(
            r.next().unwrap().unwrap(),
            Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            }
        );
        assert_eq!(
            r.next().unwrap().unwrap(),
            Element::Scalar {
                tag: Tag::Context(0),
                value: Value::Uint(42),
            }
        );
        assert_eq!(r.next().unwrap().unwrap(), Element::ContainerEnd);
        assert!(r.next().unwrap().is_none());
    }

    #[test]
    fn next_errors_on_end_of_container_at_top_level() {
        let mut r = TlvReader::new(&[0x18]);
        assert!(matches!(r.next(), Err(Error::UnexpectedEndOfContainer)));
    }

    #[test]
    fn next_errors_on_end_of_container_with_non_anonymous_tag_form() {
        // 0x38 = context-tag form (0b001) | END_OF_CONTAINER (0x18).
        let mut r = TlvReader::new(&[0x38, 0x05]);
        assert!(matches!(r.next(), Err(Error::InvalidTagControl(_))));
    }

    #[test]
    fn next_errors_on_excessive_nesting() {
        let bytes: Vec<u8> = std::iter::repeat_n(0x15u8, 33).collect();
        let mut r = TlvReader::new(&bytes);
        for _ in 0..32 {
            assert!(matches!(
                r.next().unwrap().unwrap(),
                Element::ContainerStart {
                    kind: ContainerKind::Structure,
                    ..
                },
            ));
        }
        assert!(matches!(r.next(), Err(Error::ContainerTooDeep)));
    }

    #[test]
    fn depth_returns_to_zero_after_balanced_close() {
        // Two readers — one balanced (depth returns to 0), then check that
        // a fresh reader sees 0x18 at top level as UnexpectedEndOfContainer.
        {
            let mut r = TlvReader::new(&[0x15, 0x18]);
            let _ = r.next(); // ContainerStart → depth = 1
            let _ = r.next(); // ContainerEnd → depth = 0
        }
        let mut r2 = TlvReader::new(&[0x18]);
        assert!(matches!(r2.next(), Err(Error::UnexpectedEndOfContainer)));
    }

    // --- read_value tree builder tests ---

    #[test]
    fn read_value_returns_empty_structure_vector_0019() {
        let mut r = TlvReader::new(&[0x15, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(value, Value::Structure(Vec::new()));
    }

    #[test]
    fn read_value_returns_empty_array_vector_0020() {
        let mut r = TlvReader::new(&[0x16, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(value, Value::Array(Vec::new()));
    }

    #[test]
    fn read_value_returns_structure_with_ctx_member_vector_0021() {
        let mut r = TlvReader::new(&[0x15, 0x24, 0x00, 0x2A, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(
            value,
            Value::Structure(vec![(Tag::Context(0), Value::Uint(42))])
        );
    }

    #[test]
    fn read_value_returns_array_of_three_uint8_vector_0022() {
        let mut r = TlvReader::new(&[0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(
            value,
            Value::Array(vec![Value::Uint(1), Value::Uint(2), Value::Uint(3)])
        );
    }

    #[test]
    fn read_value_returns_structure_with_bool_at_ctx7_vector_0023() {
        let mut r = TlvReader::new(&[0x15, 0x29, 0x07, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(
            value,
            Value::Structure(vec![(Tag::Context(7), Value::Bool(true))])
        );
    }

    #[test]
    fn read_value_returns_empty_list() {
        let mut r = TlvReader::new(&[0x17, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(value, Value::List(Vec::new()));
    }

    #[test]
    fn read_value_handles_nested_structure() {
        let mut r = TlvReader::new(&[0x15, 0x35, 0x00, 0x24, 0x00, 0x2A, 0x18, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        let inner = Value::Structure(vec![(Tag::Context(0), Value::Uint(42))]);
        let outer = Value::Structure(vec![(Tag::Context(0), inner)]);
        assert_eq!(value, outer);
    }

    #[test]
    fn read_value_errors_on_unclosed_container() {
        let mut r = TlvReader::new(&[0x15]);
        assert!(matches!(r.read_value(), Err(Error::UnclosedContainer)));
    }

    #[test]
    fn read_value_errors_on_dangling_end_of_container() {
        let mut r = TlvReader::new(&[0x18]);
        assert!(matches!(
            r.read_value(),
            Err(Error::UnexpectedEndOfContainer)
        ));
    }

    // --- fail-closed array tags, budget, conversion error ---

    #[test]
    fn read_value_rejects_array_with_context_tagged_child() {
        // 0x16 array-start, 0x24 0x00 0x2A = ctx(0) uint8=42, 0x18 end.
        // The child carries a context tag, which the spec forbids inside an
        // array. Pre-fix the decoder silently dropped the tag; now it errors.
        let mut r = TlvReader::new(&[0x16, 0x24, 0x00, 0x2A, 0x18]);
        assert!(matches!(r.read_value(), Err(Error::NonAnonymousArrayTag)));
    }

    #[test]
    fn read_value_rejects_array_with_context_tagged_container_child() {
        // 0x16 array-start, 0x35 0x00 = ctx(0) struct-start, 0x18 inner end,
        // 0x18 outer end. The nested container child is context-tagged.
        let mut r = TlvReader::new(&[0x16, 0x35, 0x00, 0x18, 0x18]);
        assert!(matches!(r.read_value(), Err(Error::NonAnonymousArrayTag)));
    }

    #[test]
    fn read_value_accepts_array_with_anonymous_children() {
        // Sanity: a well-formed array still decodes (regression guard for the
        // fail-closed change).
        let mut r = TlvReader::new(&[0x16, 0x04, 0x01, 0x04, 0x02, 0x18]);
        let (tag, value) = r.read_value().unwrap();
        assert_eq!(tag, Tag::Anonymous);
        assert_eq!(value, Value::Array(vec![Value::Uint(1), Value::Uint(2)]));
    }

    #[test]
    fn read_value_errors_when_element_budget_is_exceeded() {
        // Array of three uint8 children = 4 elements total (array + 3 scalars).
        // A budget of 3 cannot fit them. The 8-byte input is larger than the
        // budget, so this takes the CHARGED path (the fast path is provably
        // unreachable for a violating input: more elements than budget implies
        // more input bytes than budget).
        let bytes = [0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18];
        let mut r = TlvReader::with_element_budget(&bytes, 3);
        assert!(matches!(r.read_value(), Err(Error::ElementBudgetExceeded)));
    }

    #[test]
    fn read_value_fast_path_at_budget_equal_to_input_len() {
        // Boundary of the uncharged fast path: remaining input (8 bytes) equal
        // to the budget — the byte bound proves the 4 materialised elements
        // cannot exceed it, so the decode succeeds without accounting.
        let bytes = [0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18];
        let mut r = TlvReader::with_element_budget(&bytes, bytes.len());
        let (_, value) = r.read_value().unwrap();
        assert_eq!(
            value,
            Value::Array(vec![Value::Uint(1), Value::Uint(2), Value::Uint(3)])
        );
    }

    #[test]
    fn read_value_charged_path_at_budget_one_below_input_len() {
        // One below the fast-path boundary: input (8 bytes) > budget (7) takes
        // the charged path, which still admits the 4-element tree.
        let bytes = [0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18];
        let mut r = TlvReader::with_element_budget(&bytes, 7);
        let (_, value) = r.read_value().unwrap();
        assert_eq!(
            value,
            Value::Array(vec![Value::Uint(1), Value::Uint(2), Value::Uint(3)])
        );
    }

    #[test]
    fn read_value_succeeds_at_exactly_the_element_budget() {
        // Same input, budget of exactly 4, decodes fine.
        let bytes = [0x16, 0x04, 0x01, 0x04, 0x02, 0x04, 0x03, 0x18];
        let mut r = TlvReader::with_element_budget(&bytes, 4);
        let (_, value) = r.read_value().unwrap();
        assert_eq!(
            value,
            Value::Array(vec![Value::Uint(1), Value::Uint(2), Value::Uint(3)])
        );
    }

    #[test]
    fn read_value_budget_counts_a_single_scalar() {
        // A lone scalar costs one element; a zero budget rejects it.
        let mut r = TlvReader::with_element_budget(&[0x04, 0x2A], 0);
        assert!(matches!(r.read_value(), Err(Error::ElementBudgetExceeded)));
        let mut r = TlvReader::with_element_budget(&[0x04, 0x2A], 1);
        assert_eq!(r.read_value().unwrap(), (Tag::Anonymous, Value::Uint(42)));
    }

    #[test]
    fn fixed_width_int_decode_still_works() {
        // Guard that the InternalSliceConversion relabel did not change the
        // happy path for a fixed-width int. The conversion branch itself is
        // unreachable: `next_bytes(N)` returns exactly N bytes or `UnexpectedEof`
        // first, so the `try_into::<[u8; N]>` can never fail.
        let mut r = TlvReader::new(&[0x05, 0x34, 0x12]);
        assert_eq!(
            r.next().unwrap().unwrap(),
            Element::Scalar {
                tag: Tag::Anonymous,
                value: Value::Uint(0x1234),
            }
        );
    }

    // --- skip_container ----------------------------------------------------

    /// Build an anonymous structure: ctx0=u(7), then a nested ctx9 struct
    /// {ctx0=u(1)}, then ctx1=u(42). Returns the encoded bytes.
    fn struct_with_nested() -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = crate::writer::TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).unwrap();
        w.put_uint(Tag::Context(0), 7).unwrap();
        w.start_structure(Tag::Context(9)).unwrap();
        w.put_uint(Tag::Context(0), 1).unwrap();
        w.end_container().unwrap();
        w.put_uint(Tag::Context(1), 42).unwrap();
        w.end_container().unwrap();
        buf
    }

    #[test]
    fn read_utf8_truncates_at_is1_separator() {
        // Mirrors chip TestTLV.cpp CheckTLVCharSpan: a Matter char string is
        // presented as only the text before the first IS1 (0x1F) localized-
        // string separator. The writer keeps the raw bytes on the wire.
        fn decode_str(s: &str) -> String {
            let mut buf = Vec::new();
            let mut w = crate::writer::TlvWriter::new(&mut buf);
            w.put_utf8(Tag::Anonymous, s).unwrap();
            match TlvReader::new(&buf).next().unwrap().unwrap() {
                Element::Scalar {
                    value: Value::Utf8(t),
                    ..
                } => t,
                other => panic!("expected Utf8 scalar, got {other:?}"),
            }
        }
        // chip's two vectors: text before the separator is returned; a string
        // that STARTS with the separator presents as empty.
        assert_eq!(
            decode_str("This is a test case #1\u{1F}suffix"),
            "This is a test case #1"
        );
        assert_eq!(decode_str("\u{1F} abc \u{1F} def"), "");
        // No separator → unchanged; a real localized-label shape → just the text.
        assert_eq!(decode_str("Kitchen"), "Kitchen");
        assert_eq!(decode_str("Kitchen\u{1F}0409"), "Kitchen");
    }

    #[test]
    fn skip_container_drains_nested_struct_and_positions_after() {
        let buf = struct_with_nested();
        let mut r = TlvReader::new(&buf);
        // open the outer struct
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            })
        ));
        // consume ctx0=7
        assert!(matches!(r.next().unwrap(), Some(Element::Scalar { .. })));
        // the next element is the nested ctx9 struct — open then skip it
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            })
        ));
        r.skip_container().unwrap();
        // reader must now be positioned at ctx1=42, NOT at the outer end
        match r.next().unwrap() {
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(v),
            }) => {
                assert_eq!(v, 42);
            }
            other => panic!("expected ctx1=42 after skip, got {other:?}"),
        }
        // then the outer ContainerEnd, then None
        assert!(matches!(r.next().unwrap(), Some(Element::ContainerEnd)));
        assert!(r.next().unwrap().is_none());
    }

    #[test]
    fn skip_container_handles_array_and_list_and_empty() {
        for kind_byte in ["array", "list", "empty"] {
            let mut buf = Vec::new();
            let mut w = crate::writer::TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous).unwrap();
            match kind_byte {
                "array" => {
                    w.start_array(Tag::Context(0)).unwrap();
                    w.put_uint(Tag::Anonymous, 1).unwrap();
                    w.put_uint(Tag::Anonymous, 2).unwrap();
                    w.end_container().unwrap();
                }
                "list" => {
                    w.start_list(Tag::Context(0)).unwrap();
                    w.put_uint(Tag::Context(5), 9).unwrap();
                    w.end_container().unwrap();
                }
                _ => {
                    w.start_structure(Tag::Context(0)).unwrap();
                    w.end_container().unwrap();
                }
            }
            w.put_uint(Tag::Context(1), 99).unwrap();
            w.end_container().unwrap();

            let mut r = TlvReader::new(&buf);
            assert!(matches!(
                r.next().unwrap(),
                Some(Element::ContainerStart { .. })
            ));
            assert!(matches!(
                r.next().unwrap(),
                Some(Element::ContainerStart { .. })
            ));
            r.skip_container().unwrap();
            match r.next().unwrap() {
                Some(Element::Scalar {
                    tag: Tag::Context(1),
                    value: Value::Uint(v),
                }) => {
                    assert_eq!(v, 99, "kind {kind_byte}");
                }
                other => panic!("kind {kind_byte}: expected ctx1=99, got {other:?}"),
            }
        }
    }

    #[test]
    fn skip_container_unclosed_is_error() {
        // outer struct opened, nested struct opened but never closed (truncated)
        let mut buf = Vec::new();
        {
            let mut w = crate::writer::TlvWriter::new(&mut buf);
            w.start_structure(Tag::Anonymous).unwrap();
            w.start_structure(Tag::Context(0)).unwrap();
            w.put_uint(Tag::Anonymous, 1).unwrap();
            // deliberately do NOT close either container
        }
        let mut r = TlvReader::new(&buf);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        assert!(matches!(r.skip_container(), Err(Error::UnclosedContainer)));
    }

    // --- raw-walk skip_container ---

    #[test]
    fn skip_container_does_not_validate_skipped_utf8() {
        // Outer anon struct { ctx9 struct { utf8 with invalid byte } , ctx1=42 }.
        // Hand-assembled because the writer cannot emit invalid UTF-8.
        // 0x15                anon struct start
        //   0x35 0x09         ctx9 struct start
        //     0x0C 0x01 0xFF  utf8 len 1, invalid byte
        //   0x18              ctx9 end
        //   0x24 0x01 0x2A    ctx1 = uint8 42
        // 0x18                outer end
        let bytes = [
            0x15, 0x35, 0x09, 0x0C, 0x01, 0xFF, 0x18, 0x24, 0x01, 0x2A, 0x18,
        ];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // outer start
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart { .. })
        ));
        // Skipped (unobserved) data is NOT UTF-8 validated — a deliberate
        // loosening, like the skip that charges no element budget.
        r.skip_container().unwrap();
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(1),
                value: Value::Uint(42)
            })
        ));
    }

    #[test]
    fn skip_container_truncated_string_body_is_eof() {
        // ctx9 struct containing a utf8 claiming 5 bytes with only 2 present.
        let bytes = [0x15, 0x35, 0x09, 0x0C, 0x05, b'H', b'i'];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        r.next().unwrap();
        assert!(matches!(r.skip_container(), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn skip_container_enforces_depth_cap() {
        // 33 nested anon struct opens. Open two via next() (reader depth = 2),
        // then skip: the 31st open inside the skip sits at effective depth 32
        // and must error, exactly as the old next()-driven walk did.
        let bytes: Vec<u8> = std::iter::repeat_n(0x15u8, 33).collect();
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        r.next().unwrap();
        assert!(matches!(r.skip_container(), Err(Error::ContainerTooDeep)));
    }

    #[test]
    fn skip_container_rejects_tagged_end_marker() {
        // 0x38 = context tag-control | END_OF_CONTAINER, invalid per spec.
        let bytes = [0x15, 0x35, 0x09, 0x38, 0x05];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        r.next().unwrap();
        assert!(matches!(
            r.skip_container(),
            Err(Error::InvalidTagControl(_))
        ));
    }

    #[test]
    fn skip_container_misuse_at_top_level_errors() {
        // No container open: the walk must fail with the same error next() gives
        // for a stray end marker, and must not underflow the depth counter.
        let mut r = TlvReader::new(&[0x18]);
        assert!(matches!(
            r.skip_container(),
            Err(Error::UnexpectedEndOfContainer)
        ));
    }

    // --- next_ref zero-copy core ---

    #[test]
    fn next_ref_borrows_utf8_and_bytes() {
        let mut buf = Vec::new();
        let mut w = crate::writer::TlvWriter::new(&mut buf);
        w.put_utf8(Tag::Context(1), "Kitchen\u{1F}0409").unwrap();
        w.put_bytes(Tag::Context(2), &[0xDE, 0xAD]).unwrap();
        let mut r = TlvReader::new(&buf);
        match r.next_ref().unwrap().unwrap() {
            ElementRef::Scalar {
                tag: Tag::Context(1),
                value: ValueRef::Utf8(s),
            } => {
                // Same IS1 truncation contract as the owned path.
                assert_eq!(s, "Kitchen");
            }
            other => panic!("expected borrowed Utf8, got {other:?}"),
        }
        match r.next_ref().unwrap().unwrap() {
            ElementRef::Scalar {
                tag: Tag::Context(2),
                value: ValueRef::Bytes(b),
            } => {
                assert_eq!(b, &[0xDE, 0xAD]);
            }
            other => panic!("expected borrowed Bytes, got {other:?}"),
        }
    }

    #[test]
    fn value_ref_converts_to_owned_value() {
        assert_eq!(
            Value::from(ValueRef::Utf8("hi")),
            Value::Utf8(String::from("hi"))
        );
        assert_eq!(
            Value::from(ValueRef::Bytes(&[1, 2])),
            Value::Bytes(vec![1, 2])
        );
        assert_eq!(Value::from(ValueRef::Uint(7)), Value::Uint(7));
        assert_eq!(Value::from(ValueRef::Null), Value::Null);
    }

    // --- element spans ---

    #[test]
    fn scalar_element_span_covers_full_element_and_body_excludes_tag() {
        // ctx5 uint8=42 → bytes [0x24, 0x05, 0x2A]; body = payload only.
        let bytes = [0x24, 0x05, 0x2A];
        let mut r = TlvReader::new(&bytes);
        assert!(r.element_span().is_none(), "no element returned yet");
        r.next().unwrap().unwrap();
        let span = r.element_span().unwrap();
        assert_eq!(span.full(), 0..3);
        assert_eq!(span.body(), 2..3);
        assert_eq!(r.span_bytes(span.full()), &bytes[..]);
        assert_eq!(r.span_bytes(span.body()), &[0x2A]);
    }

    #[test]
    fn string_span_body_includes_length_field() {
        // anon utf8 "Hi" → [0x0C, 0x02, b'H', b'i']; body = len field + payload.
        let bytes = [0x0C, 0x02, b'H', b'i'];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap().unwrap();
        let span = r.element_span().unwrap();
        assert_eq!(span.full(), 0..4);
        assert_eq!(span.body(), 1..4);
    }

    #[test]
    fn skip_container_span_covers_container_and_body_excludes_header() {
        // outer anon struct { ctx9 struct { ctx1: uint8 0x2A } } sentinel.
        let buf = {
            let mut b = Vec::new();
            let mut w = crate::writer::TlvWriter::new(&mut b);
            w.start_structure(Tag::Anonymous).unwrap();
            w.start_structure(Tag::Context(9)).unwrap();
            w.put_uint(Tag::Context(1), 0x2A).unwrap();
            w.end_container().unwrap();
            w.put_uint(Tag::Context(2), 7).unwrap();
            w.end_container().unwrap();
            b
        };
        // bytes: 0x15 | 0x35 0x09 | 0x24 0x01 0x2A | 0x18 | 0x24 0x02 0x07 | 0x18
        let mut r = TlvReader::new(&buf);
        r.next().unwrap(); // outer start
        r.next().unwrap(); // ctx9 start (header span only at this point)
        let header = r.element_span().unwrap();
        assert_eq!(header.full(), 1..3, "header-only span after ContainerStart");
        let span = r.skip_container_span().unwrap();
        assert_eq!(span.full(), 1..7, "control+tag+children+end marker");
        // body EXCLUDES the container's control/tag bytes, INCLUDES its end.
        assert_eq!(r.span_bytes(span.body()), &[0x24, 0x01, 0x2A, 0x18]);
        // reader continues at the sentinel
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::Scalar {
                tag: Tag::Context(2),
                value: Value::Uint(7)
            })
        ));
    }

    #[test]
    fn retag_reemission_from_span_matches_writer_output() {
        // The 4.5 adoption pattern: copy a ctx-tagged container's body under a
        // fresh anonymous header. Must equal a writer-built anonymous
        // equivalent byte-for-byte (proves the original tag byte 0x09 is
        // excluded), and must preserve non-minimal widths in the body.
        // Hand-assembled: ctx9 struct { ctx0: uint16-encoded 42 } — the codec
        // writer would emit uint8, so bytes are assembled manually.
        let bytes = [0x35, 0x09, 0x25, 0x00, 0x2A, 0x00, 0x18];
        let mut r = TlvReader::new(&bytes);
        assert!(matches!(
            r.next().unwrap(),
            Some(Element::ContainerStart {
                kind: ContainerKind::Structure,
                ..
            })
        ));
        let span = r.skip_container_span().unwrap();
        let mut out = Vec::new();
        {
            let mut w = crate::writer::TlvWriter::new(&mut out);
            w.start_structure(Tag::Anonymous).unwrap();
        }
        out.extend_from_slice(r.span_bytes(span.body()));
        // Anonymous struct, SAME body bytes: uint16 width preserved, tag gone.
        assert_eq!(out, [0x15, 0x25, 0x00, 0x2A, 0x00, 0x18]);
    }

    #[test]
    fn skip_container_span_after_scalar_is_rejected() {
        // anon struct { ctx1: uint8 0x2A }: position the reader on the SCALAR,
        // then ask for a container span. Without the precondition guard this
        // returned Ok with a span over the scalar's bytes, which a retag caller
        // would happily re-emit as malformed TLV.
        let bytes = [0x15, 0x24, 0x01, 0x2A, 0x18];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // ContainerStart
        r.next().unwrap(); // Scalar ctx1
        assert!(matches!(
            r.skip_container_span(),
            Err(Error::UnexpectedEndOfContainer)
        ));
        // Rejected without consuming: the struct's end marker is still there.
        assert!(matches!(r.next().unwrap(), Some(Element::ContainerEnd)));
    }

    #[test]
    fn skip_container_span_before_any_element_is_rejected() {
        let mut r = TlvReader::new(&[0x15, 0x18]);
        assert!(matches!(
            r.skip_container_span(),
            Err(Error::UnexpectedEndOfContainer)
        ));
    }

    #[test]
    fn skip_container_span_twice_is_rejected() {
        // A successful skip consumes the container, so the flag must clear:
        // a second call has no ContainerStart to span.
        let bytes = [0x35, 0x09, 0x24, 0x01, 0x2A, 0x18];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // ctx9 ContainerStart
        assert!(r.skip_container_span().is_ok());
        assert!(matches!(
            r.skip_container_span(),
            Err(Error::UnexpectedEndOfContainer)
        ));
    }

    #[test]
    fn skip_container_span_after_plain_skip_is_rejected() {
        // `skip_container` also consumes the open container and so clears the
        // flag, even though it keeps its own (unguarded) contract.
        let bytes = [0x15, 0x35, 0x09, 0x18, 0x24, 0x02, 0x07, 0x18];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // outer ContainerStart
        r.next().unwrap(); // ctx9 ContainerStart
        r.skip_container().unwrap();
        assert!(matches!(
            r.skip_container_span(),
            Err(Error::UnexpectedEndOfContainer)
        ));
    }

    #[test]
    fn span_bytes_out_of_range_returns_empty() {
        let r = TlvReader::new(&[0x14]);
        assert_eq!(r.span_bytes(5..9), &[] as &[u8]);
    }

    // --- next/next_ref parity, span/depth pins ---

    /// `next()` is documented as `next_ref().map(Element::from)`. Pin that the
    /// two agree — value AND error — over hostile inputs, element by element.
    /// Errors don't all derive `PartialEq`, so compare Debug renderings.
    #[test]
    fn next_and_next_ref_agree_on_hostile_inputs() {
        // 0x0C = anonymous UTF-8 string, 1-byte length.
        let cases: &[(&[u8], &str)] = &[
            (
                &[0x0C, 0x03, 0x1F, 0x61, 0x62],
                "IS1 at index 0 -> empty string",
            ),
            (&[0x0C, 0x01, 0x1F], "IS1 only -> empty string"),
            (&[0x0C, 0x03, 0x61, 0x1F, 0xFF], "invalid UTF-8 after IS1"),
            (&[0x0C, 0x05, 0x61, 0x62], "truncated string payload"),
            (&[0x24, 0x01], "truncated scalar payload"),
            (&[0x18], "stray end-of-container"),
            (&[0x1F, 0x00], "invalid element type code"),
        ];
        for (bytes, what) in cases {
            let mut a = TlvReader::new(bytes);
            let mut b = TlvReader::new(bytes);
            loop {
                let ra = a.next();
                let rb = b.next_ref().map(|o| o.map(Element::from));
                assert_eq!(
                    format!("{ra:?}"),
                    format!("{rb:?}"),
                    "next vs next_ref diverged on: {what}"
                );
                match ra {
                    Ok(Some(_)) => {}
                    _ => break, // Ok(None) or Err: both readers are done.
                }
            }
        }
    }

    /// `next_ref`'s payloads borrow from the INPUT (`'a`), not from the
    /// reader borrow — two `ElementRef`s from successive calls coexist.
    #[test]
    fn next_ref_payloads_borrow_from_input_across_calls() {
        let mut buf = Vec::new();
        {
            let mut w = crate::writer::TlvWriter::new(&mut buf);
            w.put_utf8(Tag::Context(0), "abc").unwrap();
            w.put_bytes(Tag::Context(1), b"xyz").unwrap();
        }
        let mut r = TlvReader::new(&buf);
        let first = r.next_ref().unwrap().unwrap();
        let second = r.next_ref().unwrap().unwrap();
        // Both alive here: the 'a lifetime outlives the &mut self calls.
        let (
            ElementRef::Scalar {
                value: ValueRef::Utf8(a),
                ..
            },
            ElementRef::Scalar {
                value: ValueRef::Bytes(b),
                ..
            },
        ) = (first, second)
        else {
            panic!("unexpected shapes: {first:?} / {second:?}");
        };
        assert_eq!((a, b), ("abc", &b"xyz"[..]));
    }

    /// Plain `skip_container()` (not just the `_span` variant) must update
    /// `element_span()` to the WHOLE skipped container.
    #[test]
    fn plain_skip_container_updates_element_span_to_full_container() {
        // struct_with_nested() bytes:
        //   0: 0x15            outer struct start
        // 1-3: 0x24 0x00 0x07  ctx0 = 7
        // 4-5: 0x35 0x09       ctx9 struct start
        // 6-8: 0x24 0x00 0x01  inner ctx0 = 1
        //   9: 0x18            inner end
        // 10-12: 0x24 0x01 0x2A  ctx1 = 42
        //  13: 0x18            outer end
        let buf = struct_with_nested();
        let mut r = TlvReader::new(&buf);
        r.next().unwrap(); // outer start
        r.next().unwrap(); // ctx0 scalar
        r.next().unwrap(); // ctx9 start — header-only span
        assert_eq!(r.element_span().unwrap().full(), 4..6);
        r.skip_container().unwrap();
        let span = r.element_span().unwrap();
        assert_eq!(span.full(), 4..10, "whole container incl. end marker");
        assert_eq!(r.span_bytes(span.body()), &[0x24, 0x00, 0x01, 0x18]);
    }

    /// After a successful skip, `self.depth` is rebalanced: the outer close
    /// is consumed normally and a STRAY trailing 0x18 errors instead of
    /// closing a phantom container.
    #[test]
    fn depth_rebalanced_after_skip_rejects_stray_end_marker() {
        // outer struct { ctx9 struct {} } , outer end, then a stray end.
        let bytes = [0x15, 0x35, 0x09, 0x18, 0x18, 0x18];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // outer start (depth 1)
        r.next().unwrap(); // ctx9 start (depth 2)
        r.skip_container().unwrap(); // must leave depth == 1
        assert!(matches!(r.next(), Ok(Some(Element::ContainerEnd)))); // outer close
        assert!(matches!(r.next(), Err(Error::UnexpectedEndOfContainer)));
    }

    // ---- finish(): strict top-level close (chip MessageParser::ExitContainer) ----

    /// The happy path: every element read through the top-level close, no
    /// input left — `finish` accepts.
    #[test]
    fn finish_accepts_a_fully_read_struct() {
        // { ctx0: 1 }
        let bytes = [0x15, 0x24, 0x00, 0x01, 0x18];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // open
        r.next().unwrap(); // ctx0
        assert!(matches!(r.next(), Ok(Some(Element::ContainerEnd))));
        assert!(r.finish().is_ok());
    }

    /// A parser that stops early (it found what it wanted) may hand the
    /// reader to `finish` with containers still open: `finish` walks the
    /// unread members, nested containers included, through the top-level
    /// close.
    #[test]
    fn finish_drains_unread_members_through_the_top_level_close() {
        // { ctx0: 1, ctx1: { ctx0: 2 }, ctx2: [3] }
        let bytes = [
            0x15, 0x24, 0x00, 0x01, 0x35, 0x01, 0x24, 0x00, 0x02, 0x18, 0x36, 0x02, 0x04, 0x03,
            0x18, 0x18,
        ];
        // Stopped right after opening the outer struct.
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        assert!(r.finish().is_ok());
        // Stopped inside the nested struct (two containers open).
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // outer open
        r.next().unwrap(); // ctx0
        r.next().unwrap(); // ctx1 open
        assert!(r.finish().is_ok());
    }

    /// Truncation: the top-level end-of-container never arrives. `finish`
    /// must not treat end of input as the close.
    #[test]
    fn finish_rejects_a_missing_top_level_end_of_container() {
        // { ctx0: 1   <- no 0x18
        let bytes = [0x15, 0x24, 0x00, 0x01];
        // A caller that read until `None`, the pattern this guards against.
        let mut r = TlvReader::new(&bytes);
        while r.next().unwrap().is_some() {}
        assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
        // A caller that stopped early.
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
        // The bare struct header alone (`[0x15]`, the shortest unclosed input).
        let mut r = TlvReader::new(&bytes[..1]);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
    }

    /// Truncation inside a nested container is caught too, whether or not
    /// the caller ever opened that container.
    #[test]
    fn finish_rejects_a_missing_nested_end_of_container() {
        // { ctx1: { ctx0: 2 }   <- nested closed, outer not
        let outer_open = [0x15, 0x35, 0x01, 0x24, 0x00, 0x02, 0x18];
        let mut r = TlvReader::new(&outer_open);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
        // { ctx1: { ctx0: 2   <- neither closed
        let both_open = [0x15, 0x35, 0x01, 0x24, 0x00, 0x02];
        let mut r = TlvReader::new(&both_open);
        r.next().unwrap();
        r.next().unwrap(); // nested open: depth 2
        assert!(matches!(r.finish(), Err(Error::UnclosedContainer)));
    }

    /// A truncated scalar payload inside the unread remainder is an error,
    /// not a silent stop.
    #[test]
    fn finish_rejects_a_truncated_scalar_in_the_unread_remainder() {
        // { ctx0: u16 with one of its two payload bytes
        let bytes = [0x15, 0x25, 0x00, 0x01];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::UnexpectedEof)));
    }

    /// Bytes after the top-level end-of-container are rejected, whether the
    /// caller read through the close itself or left it to `finish`.
    #[test]
    fn finish_rejects_bytes_after_the_top_level_end_of_container() {
        // {} then a stray uint8 element
        let bytes = [0x15, 0x18, 0x04, 0x00];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap(); // open
        assert!(matches!(r.next(), Ok(Some(Element::ContainerEnd))));
        assert!(matches!(r.finish(), Err(Error::TrailingData)));
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::TrailingData)));
        // Even a single garbage byte that is not valid TLV.
        let bytes = [0x15, 0x18, 0xFF];
        let mut r = TlvReader::new(&bytes);
        r.next().unwrap();
        assert!(matches!(r.finish(), Err(Error::TrailingData)));
    }

    /// Edge cases with no container open: `finish` only asks "is the input
    /// fully consumed?".
    #[test]
    fn finish_with_no_container_open() {
        assert!(TlvReader::new(&[]).finish().is_ok());
        // Nothing consumed yet, so the whole input is trailing.
        assert!(matches!(
            TlvReader::new(&[0x15, 0x18]).finish(),
            Err(Error::TrailingData)
        ));
        // A single complete scalar read whole.
        let mut r = TlvReader::new(&[0x04, 0x07]);
        r.next().unwrap();
        assert!(r.finish().is_ok());
    }
}
