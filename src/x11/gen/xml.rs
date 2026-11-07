//! Intermediate form of an xcb protocol description.
//!
//! Documentation is skipped. `eventcopy` and `errorcopy` are replaced by the
//! source definition, keeping the copy's name and number. A list with no
//! length expression keeps [`None`], which means the list runs to the end of
//! the request. An element this parser does not know fails the parse.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    str,
};

use quick_xml::{
    events::{BytesStart, Event},
    Reader,
};

/// One xcb description file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Module {
    pub header: String,
    pub extension_xname: Option<String>,
    pub extension_name: Option<String>,
    pub major_version: Option<u16>,
    pub minor_version: Option<u16>,
    pub imports: Vec<String>,
    pub items: Vec<Item>,
}

/// A declaration, in file order. Imports live on [`Module`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Item {
    XidType(String),
    XidUnion { name: String, types: Vec<String> },
    Typedef { old_name: String, new_name: String },
    Enum { name: String, items: Vec<EnumItem> },
    Struct { name: String, fields: Vec<Field> },
    Union { name: String, fields: Vec<Field> },
    Request(Request),
    Event(EventDef),
    Error(ErrorDef),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnumItem {
    pub name: String,
    pub value: EnumValue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EnumValue {
    Value(u32),
    /// A bit position. The numeric value is `1 << bit`.
    Bit(u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Request {
    pub name: String,
    pub opcode: u8,
    pub fields: Vec<Field>,
    pub reply: Option<Vec<Field>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EventDef {
    pub name: String,
    pub number: u8,
    pub no_sequence_number: bool,
    pub xge: bool,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ErrorDef {
    pub name: String,
    pub number: u8,
    pub fields: Vec<Field>,
}

/// A named typed member, plus the enum metadata xcb attaches to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Named {
    pub name: String,
    pub type_name: String,
    pub enum_name: Option<String>,
    pub alt_enum: Option<String>,
    pub mask: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Pad {
    Bytes(u32),
    Align(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Field {
    Pad(Pad),
    Field(Named),
    List { field: Named, length: Option<Expr> },
    Expr { field: Named, expr: Expr },
    Switch { name: String, expr: Expr, cases: Vec<BitCase> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BitCase {
    pub exprs: Vec<Expr>,
    pub fields: Vec<Field>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Expr {
    FieldRef(String),
    Value(u32),
    EnumRef { enum_name: String, item: String },
    Op { op: BinOp, left: Box<Expr>, right: Box<Expr> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    BitAnd,
    Shl,
}

#[derive(Debug)]
pub(crate) struct XmlError {
    position: u64,
    message: String,
}

impl Display for XmlError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "xcb xml at byte {}: {}", self.position, self.message)
    }
}

impl Error for XmlError {}

pub(crate) fn parse(xml: &str) -> Result<Module, XmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    Parser { reader, peeked: None }.parse_module()
}

struct Parser<'a> {
    reader: Reader<&'a [u8]>,
    peeked: Option<Event<'a>>,
}

enum Pending {
    Item(Item),
    EventCopy { name: String, number: u8, source: String },
    ErrorCopy { name: String, number: u8, source: String },
}

impl<'a> Parser<'a> {
    fn parse_module(&mut self) -> Result<Module, XmlError> {
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "xcb") => return self.parse_xcb(start),
                Event::Empty(start) if is_name(&start, "xcb") => {
                    return Ok(self.header(&start, Vec::new(), Vec::new())?);
                }
                Event::Eof => return Err(self.error("missing <xcb> root")),
                event => return Err(self.unexpected(&event, "the document")),
            }
        }
    }

    fn parse_xcb(&mut self, start: BytesStart<'a>) -> Result<Module, XmlError> {
        let mut imports = Vec::new();
        let mut pending = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) => {
                    let tag = tag_name(start.local_name().as_ref());
                    match tag.as_str() {
                        "import" => imports.push(self.read_text("import")?),
                        "xidunion" => pending.push(Pending::Item(self.parse_xidunion(start)?)),
                        "enum" => pending.push(Pending::Item(self.parse_enum(start)?)),
                        "struct" => pending.push(Pending::Item(self.parse_struct(start, false)?)),
                        "union" => pending.push(Pending::Item(self.parse_struct(start, true)?)),
                        "request" => pending.push(Pending::Item(self.parse_request(start, false)?)),
                        "event" => pending.push(Pending::Item(self.parse_event(start, false)?)),
                        "error" => pending.push(Pending::Item(self.parse_error(start, false)?)),
                        "doc" => self.skip_element("doc")?,
                        other => {
                            return Err(self.error(format!("unexpected element <{other}> in <xcb>")))
                        }
                    }
                }
                Event::Empty(start) => {
                    let tag = tag_name(start.local_name().as_ref());
                    match tag.as_str() {
                        "xidtype" => {
                            pending.push(Pending::Item(Item::XidType(self.attr(&start, "name")?)))
                        }
                        "typedef" => pending.push(Pending::Item(Item::Typedef {
                            old_name: self.attr(&start, "oldname")?,
                            new_name: self.attr(&start, "newname")?,
                        })),
                        "eventcopy" => pending.push(Pending::EventCopy {
                            name: self.attr(&start, "name")?,
                            number: self.attr_u8(&start, "number")?,
                            source: self.attr(&start, "ref")?,
                        }),
                        "errorcopy" => pending.push(Pending::ErrorCopy {
                            name: self.attr(&start, "name")?,
                            number: self.attr_u8(&start, "number")?,
                            source: self.attr(&start, "ref")?,
                        }),
                        "request" => {
                            pending.push(Pending::Item(self.parse_request(start, true)?));
                        }
                        "event" => {
                            pending.push(Pending::Item(self.parse_event(start, true)?));
                        }
                        "error" => {
                            pending.push(Pending::Item(self.parse_error(start, true)?));
                        }
                        other => {
                            return Err(self.error(format!("unexpected element <{other}> in <xcb>")))
                        }
                    }
                }
                Event::End(end) if is_end(&end, "xcb") => break,
                Event::Eof => return Err(self.error("unclosed <xcb>")),
                event => return Err(self.unexpected(&event, "<xcb>")),
            }
        }
        let items = resolve(&pending).map_err(|message| self.error(message))?;
        self.header(&start, imports, items)
    }

    fn header(
        &self,
        start: &BytesStart<'a>,
        imports: Vec<String>,
        items: Vec<Item>,
    ) -> Result<Module, XmlError> {
        Ok(Module {
            header: self.attr(start, "header")?,
            extension_xname: self.opt_attr(start, "extension-xname")?,
            extension_name: self.opt_attr(start, "extension-name")?,
            major_version: self.opt_attr_u16(start, "major-version")?,
            minor_version: self.opt_attr_u16(start, "minor-version")?,
            imports,
            items,
        })
    }

    fn parse_xidunion(&mut self, start: BytesStart<'a>) -> Result<Item, XmlError> {
        let name = self.attr(&start, "name")?;
        let mut types = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "type") => {
                    types.push(self.read_text("type")?);
                }
                Event::End(end) if is_end(&end, "xidunion") => {
                    if types.is_empty() {
                        return Err(self.error(format!("<xidunion {name}> has no types")));
                    }
                    return Ok(Item::XidUnion { name, types });
                }
                Event::Eof => return Err(self.error("unclosed <xidunion>")),
                event => return Err(self.unexpected(&event, "<xidunion>")),
            }
        }
    }

    fn parse_enum(&mut self, start: BytesStart<'a>) -> Result<Item, XmlError> {
        let name = self.attr(&start, "name")?;
        let mut items = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "item") => {
                    items.push(self.parse_enum_item(start)?);
                }
                Event::Start(start) if is_name(&start, "doc") => self.skip_element("doc")?,
                Event::End(end) if is_end(&end, "enum") => {
                    if items.is_empty() {
                        return Err(self.error(format!("<enum {name}> has no items")));
                    }
                    return Ok(Item::Enum { name, items });
                }
                Event::Eof => return Err(self.error("unclosed <enum>")),
                event => return Err(self.unexpected(&event, "<enum>")),
            }
        }
    }

    fn parse_enum_item(&mut self, start: BytesStart<'a>) -> Result<EnumItem, XmlError> {
        let name = self.attr(&start, "name")?;
        let value = match self.next()? {
            Event::Start(start) if is_name(&start, "value") => {
                let text = self.read_text("value")?;
                EnumValue::Value(self.parse_u32(&text, "value")?)
            }
            Event::Start(start) if is_name(&start, "bit") => {
                let text = self.read_text("bit")?;
                let bit = self.parse_u32(&text, "bit")?;
                if bit >= 32 {
                    return Err(self.error(format!("bit {bit} does not fit in 32 bits")));
                }
                EnumValue::Bit(bit as u8)
            }
            event => return Err(self.unexpected(&event, "<item>")),
        };
        self.expect_end("item")?;
        Ok(EnumItem { name, value })
    }

    fn parse_struct(&mut self, start: BytesStart<'a>, is_union: bool) -> Result<Item, XmlError> {
        let name = self.attr(&start, "name")?;
        let fields = self.parse_fields(if is_union { "union" } else { "struct" })?;
        if is_union {
            Ok(Item::Union { name, fields })
        } else {
            Ok(Item::Struct { name, fields })
        }
    }

    fn parse_request(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Item, XmlError> {
        let name = self.attr(&start, "name")?;
        let opcode = self.attr_u8(&start, "opcode")?;
        if closed {
            return Ok(Item::Request(Request { name, opcode, fields: Vec::new(), reply: None }));
        }
        let mut fields = Vec::new();
        let mut reply = None;
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "reply") => {
                    if reply.is_some() {
                        return Err(self.error(format!("<{name}> has two replies")));
                    }
                    reply = Some(self.parse_fields("reply")?);
                }
                Event::Start(start) if is_name(&start, "doc") => self.skip_element("doc")?,
                Event::Start(start) => fields.push(self.parse_content(start, false)?),
                Event::Empty(start) => fields.push(self.parse_content(start, true)?),
                Event::End(end) if is_end(&end, "request") => break,
                Event::Eof => return Err(self.error("unclosed <request>")),
                event => return Err(self.unexpected(&event, "<request>")),
            }
        }
        Ok(Item::Request(Request { name, opcode, fields, reply }))
    }

    fn parse_event(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Item, XmlError> {
        let event = EventDef {
            name: self.attr(&start, "name")?,
            number: self.attr_u8(&start, "number")?,
            no_sequence_number: self.flag(&start, "no-sequence-number")?,
            xge: self.flag(&start, "xge")?,
            fields: if closed { Vec::new() } else { self.parse_fields("event")? },
        };
        Ok(Item::Event(event))
    }

    fn parse_error(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Item, XmlError> {
        let error = ErrorDef {
            name: self.attr(&start, "name")?,
            number: self.attr_u8(&start, "number")?,
            fields: if closed { Vec::new() } else { self.parse_fields("error")? },
        };
        Ok(Item::Error(error))
    }

    fn parse_fields(&mut self, end_tag: &str) -> Result<Vec<Field>, XmlError> {
        let mut fields = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "doc") => self.skip_element("doc")?,
                Event::Start(start) => fields.push(self.parse_content(start, false)?),
                Event::Empty(start) => fields.push(self.parse_content(start, true)?),
                Event::End(end) if is_end(&end, end_tag) => return Ok(fields),
                Event::Eof => return Err(self.error(format!("unclosed <{end_tag}>"))),
                event => return Err(self.unexpected(&event, &format!("<{end_tag}>"))),
            }
        }
    }

    fn parse_content(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Field, XmlError> {
        let tag = tag_name(start.local_name().as_ref());
        match tag.as_str() {
            "pad" => {
                if !closed {
                    self.expect_end("pad")?;
                }
                Ok(Field::Pad(self.pad(&start)?))
            }
            "field" => {
                if !closed {
                    self.expect_end("field")?;
                }
                Ok(Field::Field(self.named(&start)?))
            }
            "list" => self.finish_list(start, closed),
            "exprfield" => {
                if closed {
                    return Err(self.error("empty <exprfield>"));
                }
                let field = self.named(&start)?;
                let expr = self.parse_expr()?;
                self.expect_end("exprfield")?;
                Ok(Field::Expr { field, expr })
            }
            "switch" => {
                if closed {
                    return Err(self.error("empty <switch>"));
                }
                self.finish_switch(start)
            }
            other => Err(self.error(format!("unexpected element <{other}> in a field list"))),
        }
    }

    fn finish_list(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Field, XmlError> {
        let field = self.named(&start)?;
        if closed || self.at_end("list")? {
            if !closed {
                self.expect_end("list")?;
            }
            return Ok(Field::List { field, length: None });
        }
        let length = Some(self.parse_expr()?);
        self.expect_end("list")?;
        Ok(Field::List { field, length })
    }

    fn finish_switch(&mut self, start: BytesStart<'a>) -> Result<Field, XmlError> {
        let name = self.attr(&start, "name")?;
        let expr = self.parse_expr()?;
        let mut cases = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "bitcase") || is_name(&start, "case") => {
                    cases.push(self.parse_case()?);
                }
                Event::End(end) if is_end(&end, "switch") => break,
                Event::Eof => return Err(self.error("unclosed <switch>")),
                event => return Err(self.unexpected(&event, "<switch>")),
            }
        }
        if cases.is_empty() {
            return Err(self.error(format!("<switch {name}> has no cases")));
        }
        Ok(Field::Switch { name, expr, cases })
    }

    fn parse_case(&mut self) -> Result<BitCase, XmlError> {
        let mut exprs = Vec::new();
        let mut fields = Vec::new();
        loop {
            match self.next()? {
                Event::Start(start) if is_name(&start, "doc") => self.skip_element("doc")?,
                Event::Start(start) if fields.is_empty() && is_expr_tag(&start) => {
                    exprs.push(self.expr_from(start, false)?);
                }
                Event::Empty(start) if fields.is_empty() && is_expr_tag(&start) => {
                    exprs.push(self.expr_from(start, true)?);
                }
                Event::Start(start) => fields.push(self.parse_content(start, false)?),
                Event::Empty(start) => fields.push(self.parse_content(start, true)?),
                Event::End(end) if is_end(&end, "bitcase") || is_end(&end, "case") => {
                    if exprs.is_empty() {
                        return Err(self.error("a switch case has no expression"));
                    }
                    return Ok(BitCase { exprs, fields });
                }
                Event::Eof => return Err(self.error("unclosed switch case")),
                event => return Err(self.unexpected(&event, "a switch case")),
            }
        }
    }

    fn parse_expr(&mut self) -> Result<Expr, XmlError> {
        match self.next()? {
            Event::Start(start) => self.expr_from(start, false),
            Event::Empty(start) => self.expr_from(start, true),
            event => Err(self.unexpected(&event, "an expression")),
        }
    }

    fn expr_from(&mut self, start: BytesStart<'a>, closed: bool) -> Result<Expr, XmlError> {
        let tag = tag_name(start.local_name().as_ref());
        match tag.as_str() {
            "unop" | "popcount" | "sumof" | "paramref" | "listelement-ref" | "bit" => {
                Err(self.error(format!("unsupported expression <{tag}>")))
            }
            "fieldref" => {
                if closed {
                    return Err(self.error("empty <fieldref>"));
                }
                Ok(Expr::FieldRef(self.read_text("fieldref")?))
            }
            "value" => {
                if closed {
                    return Err(self.error("empty <value>"));
                }
                let text = self.read_text("value")?;
                Ok(Expr::Value(self.parse_u32(&text, "value")?))
            }
            "enumref" => {
                if closed {
                    return Err(self.error("empty <enumref>"));
                }
                let enum_name = self.attr(&start, "ref")?;
                Ok(Expr::EnumRef { enum_name, item: self.read_text("enumref")? })
            }
            "op" => {
                if closed {
                    return Err(self.error("empty <op>"));
                }
                let op = self.bin_op(&self.attr(&start, "op")?)?;
                let left = Box::new(self.parse_expr()?);
                let right = Box::new(self.parse_expr()?);
                self.expect_end("op")?;
                Ok(Expr::Op { op, left, right })
            }
            other => Err(self.error(format!("unexpected element <{other}> in an expression"))),
        }
    }

    fn bin_op(&self, op: &str) -> Result<BinOp, XmlError> {
        Ok(match op {
            "+" => BinOp::Add,
            "-" => BinOp::Sub,
            "*" => BinOp::Mul,
            "/" => BinOp::Div,
            "&" => BinOp::BitAnd,
            "<<" => BinOp::Shl,
            other => return Err(self.error(format!("unknown operator `{other}`"))),
        })
    }

    fn pad(&self, start: &BytesStart<'a>) -> Result<Pad, XmlError> {
        match (self.opt_attr(start, "bytes")?, self.opt_attr(start, "align")?) {
            (Some(bytes), None) => Ok(Pad::Bytes(self.parse_u32(&bytes, "pad bytes")?)),
            (None, Some(align)) => Ok(Pad::Align(self.parse_u32(&align, "pad align")?)),
            _ => Err(self.error("<pad> needs exactly one of bytes or align")),
        }
    }

    fn named(&self, start: &BytesStart<'a>) -> Result<Named, XmlError> {
        Ok(Named {
            name: self.attr(start, "name")?,
            type_name: self.attr(start, "type")?,
            enum_name: self.opt_attr(start, "enum")?,
            alt_enum: self.opt_attr(start, "altenum")?,
            mask: self.opt_attr(start, "mask")?,
        })
    }

    fn read_text(&mut self, end_tag: &str) -> Result<String, XmlError> {
        let mut text = String::new();
        loop {
            match self.next()? {
                Event::Text(chunk) => {
                    text.push_str(&chunk.decode().map_err(|err| self.error(err.to_string()))?);
                }
                Event::CData(chunk) => {
                    text.push_str(
                        str::from_utf8(chunk.as_ref())
                            .map_err(|err| self.error(err.to_string()))?,
                    );
                }
                Event::End(end) if is_end(&end, end_tag) => return Ok(text),
                event => return Err(self.unexpected(&event, &format!("<{end_tag}>"))),
            }
        }
    }

    fn expect_end(&mut self, tag: &str) -> Result<(), XmlError> {
        match self.next()? {
            Event::End(end) if is_end(&end, tag) => Ok(()),
            event => Err(self.unexpected(&event, &format!("the end of <{tag}>"))),
        }
    }

    fn skip_element(&mut self, tag: &str) -> Result<(), XmlError> {
        let mut depth = 1u32;
        while depth > 0 {
            match self.next()? {
                Event::Start(start) if is_name(&start, tag) => depth += 1,
                Event::End(end) if is_end(&end, tag) => depth -= 1,
                Event::Eof => return Err(self.error(format!("unclosed <{tag}>"))),
                _ => {}
            }
        }
        Ok(())
    }

    fn at_end(&mut self, tag: &str) -> Result<bool, XmlError> {
        Ok(matches!(self.peek()?, Event::End(end) if is_end(end, tag)))
    }

    fn peek(&mut self) -> Result<&Event<'a>, XmlError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.read_event()?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    fn next(&mut self) -> Result<Event<'a>, XmlError> {
        if let Some(event) = self.peeked.take() {
            return Ok(event);
        }
        self.read_event()
    }

    fn read_event(&mut self) -> Result<Event<'a>, XmlError> {
        loop {
            match self.reader.read_event() {
                Ok(Event::Comment(_) | Event::Decl(_) | Event::DocType(_) | Event::PI(_)) => {}
                Ok(event) => return Ok(event),
                Err(err) => {
                    let position = self.reader.error_position();
                    return Err(XmlError { position, message: err.to_string() });
                }
            }
        }
    }

    fn attr(&self, start: &BytesStart<'_>, key: &str) -> Result<String, XmlError> {
        self.opt_attr(start, key)?
            .ok_or_else(|| self.error(format!("missing attribute `{key}`")))
    }

    fn opt_attr(&self, start: &BytesStart<'_>, key: &str) -> Result<Option<String>, XmlError> {
        let Some(attr) = start
            .try_get_attribute(key)
            .map_err(|err| self.error(err.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(
            attr.unescape_value()
                .map_err(|err| self.error(err.to_string()))?
                .into_owned(),
        ))
    }

    fn attr_u8(&self, start: &BytesStart<'_>, key: &str) -> Result<u8, XmlError> {
        let text = self.attr(start, key)?;
        text.parse::<u8>()
            .map_err(|_| self.error(format!("invalid {key} `{text}`")))
    }

    fn opt_attr_u16(&self, start: &BytesStart<'_>, key: &str) -> Result<Option<u16>, XmlError> {
        let Some(text) = self.opt_attr(start, key)? else {
            return Ok(None);
        };
        text.parse::<u16>()
            .map(Some)
            .map_err(|_| self.error(format!("invalid {key} `{text}`")))
    }

    fn flag(&self, start: &BytesStart<'_>, key: &str) -> Result<bool, XmlError> {
        match self.opt_attr(start, key)?.as_deref() {
            None | Some("false") => Ok(false),
            Some("true") => Ok(true),
            Some(value) => Err(self.error(format!("invalid {key} `{value}`"))),
        }
    }

    fn parse_u32(&self, text: &str, what: &str) -> Result<u32, XmlError> {
        text.parse::<u32>()
            .map_err(|_| self.error(format!("invalid {what} `{text}`")))
    }

    fn error(&self, message: impl Into<String>) -> XmlError {
        XmlError {
            position: self.reader.buffer_position(),
            message: message.into(),
        }
    }

    fn unexpected(&self, event: &Event<'_>, where_: &str) -> XmlError {
        let found = match event {
            Event::Start(start) | Event::Empty(start) => {
                format!("<{}>", tag_name(start.local_name().as_ref()))
            }
            Event::End(end) => format!("</{}>", tag_name(end.local_name().as_ref())),
            Event::Text(_) | Event::CData(_) => "text".to_owned(),
            Event::Eof => "end of file".to_owned(),
            _ => "xml event".to_owned(),
        };
        self.error(format!("unexpected {found} in {where_}"))
    }
}

fn is_name(start: &BytesStart<'_>, expected: &str) -> bool {
    start.local_name().as_ref() == expected.as_bytes()
}

fn is_end(end: &quick_xml::events::BytesEnd<'_>, expected: &str) -> bool {
    end.local_name().as_ref() == expected.as_bytes()
}

fn is_expr_tag(start: &BytesStart<'_>) -> bool {
    matches!(
        start.local_name().as_ref(),
        b"fieldref"
            | b"value"
            | b"op"
            | b"enumref"
            | b"unop"
            | b"popcount"
            | b"sumof"
            | b"paramref"
            | b"listelement-ref"
            | b"bit"
    )
}

fn tag_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn resolve(pending: &[Pending]) -> Result<Vec<Item>, String> {
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let mut event_copies = Vec::new();
    let mut error_copies = Vec::new();
    for item in pending {
        match item {
            Pending::Item(Item::Event(event)) => events.push(event),
            Pending::Item(Item::Error(error)) => errors.push(error),
            Pending::EventCopy { name, number, source } => {
                event_copies.push((name.as_str(), *number, source.as_str()))
            }
            Pending::ErrorCopy { name, number, source } => {
                error_copies.push((name.as_str(), *number, source.as_str()))
            }
            Pending::Item(_) => {}
        }
    }
    pending
        .iter()
        .map(|item| resolve_one(item, &events, &errors, &event_copies, &error_copies))
        .collect()
}

fn resolve_one(
    item: &Pending,
    events: &[&EventDef],
    errors: &[&ErrorDef],
    event_copies: &[(&str, u8, &str)],
    error_copies: &[(&str, u8, &str)],
) -> Result<Item, String> {
    match item {
        Pending::Item(item) => Ok(item.clone()),
        Pending::EventCopy { name, number, source } => {
            let source = concrete_event(source, events, event_copies, &mut vec![name.clone()])?;
            Ok(Item::Event(EventDef {
                name: name.clone(),
                number: *number,
                no_sequence_number: source.no_sequence_number,
                xge: source.xge,
                fields: source.fields.clone(),
            }))
        }
        Pending::ErrorCopy { name, number, source } => {
            let source = concrete_error(source, errors, error_copies, &mut vec![name.clone()])?;
            Ok(Item::Error(ErrorDef {
                name: name.clone(),
                number: *number,
                fields: source.fields.clone(),
            }))
        }
    }
}

fn concrete_event<'a>(
    name: &str,
    events: &[&'a EventDef],
    copies: &[(&str, u8, &str)],
    stack: &mut Vec<String>,
) -> Result<&'a EventDef, String> {
    if let Some(event) = events.iter().find(|event| event.name == name) {
        return Ok(*event);
    }
    let Some((_, _, source)) = copies.iter().find(|(copy_name, _, _)| *copy_name == name) else {
        return Err(format!("eventcopy references unknown event {name}"));
    };
    if stack.iter().any(|seen| seen == name) {
        return Err(format!("eventcopy cycle involving {name}"));
    }
    stack.push(name.to_owned());
    concrete_event(source, events, copies, stack)
}

fn concrete_error<'a>(
    name: &str,
    errors: &[&'a ErrorDef],
    copies: &[(&str, u8, &str)],
    stack: &mut Vec<String>,
) -> Result<&'a ErrorDef, String> {
    if let Some(error) = errors.iter().find(|error| error.name == name) {
        return Ok(*error);
    }
    let Some((_, _, source)) = copies.iter().find(|(copy_name, _, _)| *copy_name == name) else {
        return Err(format!("errorcopy references unknown error {name}"));
    };
    if stack.iter().any(|seen| seen == name) {
        return Err(format!("errorcopy cycle involving {name}"));
    }
    stack.push(name.to_owned());
    concrete_error(source, errors, copies, stack)
}

#[cfg(test)]
mod tests {
    use super::{parse, BinOp, EnumValue, Expr, Field, Item, Module, Named, Pad, Request};

    fn load(name: &str) -> Module {
        let path = format!("/usr/share/xcb/{name}.xml");
        let xml = std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{path}: {err}"));
        parse(&xml).unwrap_or_else(|err| panic!("{path}: {err}"))
    }

    fn request<'a>(module: &'a Module, name: &str) -> &'a Request {
        module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Request(request) if request.name == name => Some(request),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing request {name}"))
    }

    fn fields_named<'a>(module: &'a Module, kind: &str, name: &str) -> &'a [Field] {
        module
            .items
            .iter()
            .find_map(|item| match (kind, item) {
                ("struct", Item::Struct { name: item_name, fields })
                | ("union", Item::Union { name: item_name, fields })
                    if item_name == name =>
                {
                    Some(fields.as_slice())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing {kind} {name}"))
    }

    fn plain(name: &str, type_name: &str) -> Field {
        Field::Field(Named {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            enum_name: None,
            alt_enum: None,
            mask: None,
        })
    }

    #[test]
    fn the_installed_descriptions_parse() {
        for name in ["xproto", "composite", "xfixes", "res", "render"] {
            assert!(!load(name).items.is_empty(), "{name}");
        }
    }

    #[test]
    fn intern_atom_keeps_its_fields_and_reply() {
        let xproto = load("xproto");
        let request = request(&xproto, "InternAtom");
        assert_eq!(request.opcode, 16);
        assert_eq!(
            request.fields,
            vec![
                plain("only_if_exists", "BOOL"),
                plain("name_len", "CARD16"),
                Field::Pad(Pad::Bytes(2)),
                Field::List {
                    field: Named {
                        name: "name".to_owned(),
                        type_name: "char".to_owned(),
                        enum_name: None,
                        alt_enum: None,
                        mask: None,
                    },
                    length: Some(Expr::FieldRef("name_len".to_owned())),
                },
            ]
        );
        let reply = request.reply.as_deref().unwrap();
        assert_eq!(
            reply,
            &[
                Field::Pad(Pad::Bytes(1)),
                Field::Field(Named {
                    name: "atom".to_owned(),
                    type_name: "ATOM".to_owned(),
                    enum_name: None,
                    alt_enum: Some("Atom".to_owned()),
                    mask: None,
                }),
            ]
        );
    }

    #[test]
    fn get_property_reply_keeps_the_value_length_expression() {
        let xproto = load("xproto");
        let reply = request(&xproto, "GetProperty").reply.as_deref().unwrap();
        let Field::List { field, length } = reply.last().unwrap() else {
            panic!("value list missing");
        };
        assert_eq!(field.name, "value");
        assert_eq!(field.type_name, "void");
        assert_eq!(
            length,
            &Some(Expr::Op {
                op: BinOp::Mul,
                left: Box::new(Expr::FieldRef("value_len".to_owned())),
                right: Box::new(Expr::Op {
                    op: BinOp::Div,
                    left: Box::new(Expr::FieldRef("format".to_owned())),
                    right: Box::new(Expr::Value(8)),
                }),
            })
        );
    }

    #[test]
    fn configure_window_switch_lists_every_mask_bit() {
        let xproto = load("xproto");
        let request = request(&xproto, "ConfigureWindow");
        let mask = match &request.fields[2] {
            Field::Field(field) => field,
            other => panic!("value mask missing: {other:?}"),
        };
        assert_eq!(mask.name, "value_mask");
        assert_eq!(mask.mask.as_deref(), Some("ConfigWindow"));
        let Field::Switch { name, expr, cases } = request.fields.last().unwrap() else {
            panic!("switch missing");
        };
        assert_eq!(name, "value_list");
        assert_eq!(expr, &Expr::FieldRef("value_mask".to_owned()));
        let expected = [
            ("X", "INT32", "x", None, None),
            ("Y", "INT32", "y", None, None),
            ("Width", "CARD32", "width", None, None),
            ("Height", "CARD32", "height", None, None),
            ("BorderWidth", "CARD32", "border_width", None, None),
            ("Sibling", "WINDOW", "sibling", None, Some("Window")),
            ("StackMode", "CARD32", "stack_mode", Some("StackMode"), None),
        ];
        assert_eq!(cases.len(), expected.len());
        for (case, (item, type_name, field_name, enum_name, alt_enum)) in cases.iter().zip(expected)
        {
            assert_eq!(
                case.exprs,
                vec![Expr::EnumRef { enum_name: "ConfigWindow".to_owned(), item: item.to_owned() }]
            );
            let Field::Field(field) = &case.fields[0] else { panic!("case field") };
            assert_eq!(field.name, field_name);
            assert_eq!(field.type_name, type_name);
            assert_eq!(field.enum_name.as_deref(), enum_name);
            assert_eq!(field.alt_enum.as_deref(), alt_enum);
        }
    }

    #[test]
    fn client_message_data_is_three_fixed_lists() {
        let xproto = load("xproto");
        let fields = fields_named(&xproto, "union", "ClientMessageData");
        let lists: Vec<_> = fields
            .iter()
            .map(|field| match field {
                Field::List { field, length } => {
                    (field.name.as_str(), field.type_name.as_str(), length.clone())
                }
                other => panic!("unexpected union member {other:?}"),
            })
            .collect();
        assert_eq!(
            lists,
            vec![
                ("data8", "CARD8", Some(Expr::Value(20))),
                ("data16", "CARD16", Some(Expr::Value(10))),
                ("data32", "CARD32", Some(Expr::Value(5))),
            ]
        );
    }

    #[test]
    fn setup_lists_nest_through_screen_and_depth() {
        let xproto = load("xproto");
        let setup = fields_named(&xproto, "struct", "Setup");
        let lists: Vec<_> = setup
            .iter()
            .filter_map(|field| match field {
                Field::List { field, length: Some(Expr::FieldRef(referent)) } => {
                    Some((field.name.as_str(), field.type_name.as_str(), referent.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            lists,
            vec![
                ("vendor", "char", "vendor_len"),
                ("pixmap_formats", "FORMAT", "pixmap_formats_len"),
                ("roots", "SCREEN", "roots_len"),
            ]
        );
        assert!(setup
            .iter()
            .any(|field| field == &Field::Pad(Pad::Align(4))));

        let screen = fields_named(&xproto, "struct", "SCREEN");
        assert!(screen.iter().any(|field| matches!(
            field,
            Field::List { field, length: Some(Expr::FieldRef(referent)) }
                if field.name == "allowed_depths"
                    && field.type_name == "DEPTH"
                    && referent == "allowed_depths_len"
        )));
        let depth = fields_named(&xproto, "struct", "DEPTH");
        assert!(depth.iter().any(|field| matches!(
            field,
            Field::List { field, length: Some(Expr::FieldRef(referent)) }
                if field.name == "visuals"
                    && field.type_name == "VISUALTYPE"
                    && referent == "visuals_len"
        )));
    }

    #[test]
    fn extension_headers_carry_names_versions_and_imports() {
        let xproto = load("xproto");
        assert_eq!(xproto.header, "xproto");
        assert_eq!(xproto.extension_xname, None);
        assert_eq!(xproto.extension_name, None);
        assert_eq!(xproto.major_version, None);
        assert_eq!(xproto.minor_version, None);
        assert!(xproto.imports.is_empty());

        let expected = [
            ("composite", "composite", "Composite", "Composite", 0, 4, &["xproto", "xfixes"][..]),
            ("xfixes", "xfixes", "XFIXES", "XFixes", 6, 0, &["xproto", "render", "shape"][..]),
            ("render", "render", "RENDER", "Render", 0, 11, &["xproto"][..]),
            ("res", "res", "X-Resource", "Res", 1, 2, &["xproto"][..]),
        ];
        for (file, header, xname, extension, major, minor, imports) in expected {
            let module = load(file);
            assert_eq!(module.header, header);
            assert_eq!(module.extension_xname.as_deref(), Some(xname));
            assert_eq!(module.extension_name.as_deref(), Some(extension));
            assert_eq!(module.major_version, Some(major));
            assert_eq!(module.minor_version, Some(minor));
            assert_eq!(module.imports, imports);
        }
    }

    #[test]
    fn copies_take_their_own_number_and_the_source_fields() {
        let xproto = load("xproto");
        let event = |name: &str| {
            xproto
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Event(event) if event.name == name => Some(event),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing event {name}"))
        };
        let error = |name: &str| {
            xproto
                .items
                .iter()
                .find_map(|item| match item {
                    Item::Error(error) if error.name == name => Some(error),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing error {name}"))
        };
        let focus_in = event("FocusIn");
        let focus_out = event("FocusOut");
        assert_eq!(focus_out.number, 10);
        assert_eq!(focus_out.fields, focus_in.fields);
        assert!(!focus_out.no_sequence_number);
        assert!(event("KeymapNotify").no_sequence_number);
        assert!(event("GeGeneric").xge);

        let window = error("Window");
        assert_eq!(window.number, 3);
        assert_eq!(window.fields, error("Value").fields);
    }

    #[test]
    fn enums_keep_plain_values_and_bit_positions() {
        let xproto = load("xproto");
        let items = xproto
            .items
            .iter()
            .find_map(|item| match item {
                Item::Enum { name, items } if name == "EventMask" => Some(items),
                _ => None,
            })
            .unwrap();
        assert_eq!(items[0].name, "NoEvent");
        assert_eq!(items[0].value, EnumValue::Value(0));
        assert_eq!(items[1].name, "KeyPress");
        assert_eq!(items[1].value, EnumValue::Bit(0));
    }

    #[test]
    fn an_expression_field_and_a_list_without_a_length_are_kept() {
        let xproto = load("xproto");
        let request = request(&xproto, "QueryTextExtents");
        let Field::Expr { field, expr } = &request.fields[0] else {
            panic!("exprfield missing");
        };
        assert_eq!(field.name, "odd_length");
        assert_eq!(field.type_name, "BOOL");
        assert_eq!(
            expr,
            &Expr::Op {
                op: BinOp::BitAnd,
                left: Box::new(Expr::FieldRef("string_len".to_owned())),
                right: Box::new(Expr::Value(1)),
            }
        );
        assert!(request.fields.iter().any(|field| matches!(
            field,
            Field::List { field, length: None } if field.name == "string" && field.type_name == "CHAR2B"
        )));
    }

    #[test]
    fn documentation_is_not_a_protocol_field() {
        let module = parse(
            r#"<xcb header="x">
                <request name="InternAtom" opcode="16">
                    <field type="BOOL" name="only_if_exists"/>
                    <doc><field name="only_if_exists">ignored</field></doc>
                </request>
            </xcb>"#,
        )
        .unwrap();
        let Item::Request(request) = &module.items[0] else { panic!("request") };
        assert_eq!(request.fields, vec![plain("only_if_exists", "BOOL")]);
    }

    #[test]
    fn an_unknown_element_is_rejected() {
        let error = parse(r#"<xcb header="x"><fd name="fence"/></xcb>"#).unwrap_err();
        assert!(error.to_string().contains("<fd>"), "{error}");
    }

    #[test]
    fn a_copy_of_an_unknown_event_is_rejected() {
        let error = parse(
            r#"<xcb header="x"><eventcopy name="FocusOut" number="10" ref="FocusIn"/></xcb>"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("FocusIn"), "{error}");
    }
}
