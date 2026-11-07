//! Emits the checked-in X11 protocol in `src/x11/generated.rs`.
//!
//! The selection list is the set of requests, events, and declarations later
//! phases need. Types, requests, replies, events, and errors from that set are emitted.

use std::collections::{HashMap, HashSet};

use heck::ToUpperCamelCase;

use super::xml::{
    parse, BinOp, BitCase, EnumItem, EnumValue, Expr, Field, Item, Module, Named, Pad,
};

const XCB_DIR: &str = "/usr/share/xcb";

const MODULE_FILES: &[&str] = &["xproto", "composite", "xfixes", "res", "render"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionKind {
    Request,
    Event,
    Decl,
}

/// Protocol items whose types this generator closes over.
pub(crate) const SELECTION: &[(&str, &str, SelectionKind)] = &[
    ("xproto", "CreateWindow", SelectionKind::Request),
    ("xproto", "ChangeWindowAttributes", SelectionKind::Request),
    ("xproto", "GetWindowAttributes", SelectionKind::Request),
    ("xproto", "DestroyWindow", SelectionKind::Request),
    ("xproto", "MapWindow", SelectionKind::Request),
    ("xproto", "UnmapWindow", SelectionKind::Request),
    ("xproto", "ConfigureWindow", SelectionKind::Request),
    ("xproto", "GetGeometry", SelectionKind::Request),
    ("xproto", "QueryTree", SelectionKind::Request),
    ("xproto", "InternAtom", SelectionKind::Request),
    ("xproto", "GetAtomName", SelectionKind::Request),
    ("xproto", "ChangeProperty", SelectionKind::Request),
    ("xproto", "DeleteProperty", SelectionKind::Request),
    ("xproto", "GetProperty", SelectionKind::Request),
    ("xproto", "SetSelectionOwner", SelectionKind::Request),
    ("xproto", "GetSelectionOwner", SelectionKind::Request),
    ("xproto", "ConvertSelection", SelectionKind::Request),
    ("xproto", "SendEvent", SelectionKind::Request),
    ("xproto", "SetInputFocus", SelectionKind::Request),
    ("xproto", "GetInputFocus", SelectionKind::Request),
    ("xproto", "QueryExtension", SelectionKind::Request),
    ("xproto", "KillClient", SelectionKind::Request),
    ("xproto", "CreatePixmap", SelectionKind::Request),
    ("xproto", "FreePixmap", SelectionKind::Request),
    ("xproto", "CreateGC", SelectionKind::Request),
    ("xproto", "FreeGC", SelectionKind::Request),
    ("xproto", "PutImage", SelectionKind::Request),
    ("xproto", "CreateNotify", SelectionKind::Event),
    ("xproto", "DestroyNotify", SelectionKind::Event),
    ("xproto", "UnmapNotify", SelectionKind::Event),
    ("xproto", "MapNotify", SelectionKind::Event),
    ("xproto", "MapRequest", SelectionKind::Event),
    ("xproto", "ReparentNotify", SelectionKind::Event),
    ("xproto", "ConfigureNotify", SelectionKind::Event),
    ("xproto", "ConfigureRequest", SelectionKind::Event),
    ("xproto", "PropertyNotify", SelectionKind::Event),
    ("xproto", "SelectionClear", SelectionKind::Event),
    ("xproto", "SelectionRequest", SelectionKind::Event),
    ("xproto", "SelectionNotify", SelectionKind::Event),
    ("xproto", "ClientMessage", SelectionKind::Event),
    ("xproto", "FocusIn", SelectionKind::Event),
    ("xproto", "FocusOut", SelectionKind::Event),
    ("xproto", "SetupRequest", SelectionKind::Decl),
    ("xproto", "Setup", SelectionKind::Decl),
    ("xproto", "SetupFailed", SelectionKind::Decl),
    ("xproto", "SetupAuthenticate", SelectionKind::Decl),
    ("xproto", "Atom", SelectionKind::Decl),
    ("xproto", "EventMask", SelectionKind::Decl),
    ("xproto", "CW", SelectionKind::Decl),
    ("xproto", "ConfigWindow", SelectionKind::Decl),
    ("xproto", "GC", SelectionKind::Decl),
    ("composite", "QueryVersion", SelectionKind::Request),
    ("composite", "RedirectSubwindows", SelectionKind::Request),
    ("xfixes", "QueryVersion", SelectionKind::Request),
    ("xfixes", "SelectSelectionInput", SelectionKind::Request),
    ("xfixes", "SelectionNotify", SelectionKind::Event),
    ("res", "QueryVersion", SelectionKind::Request),
    ("res", "QueryClientIds", SelectionKind::Request),
    ("render", "QueryVersion", SelectionKind::Request),
    ("render", "QueryPictFormats", SelectionKind::Request),
    ("render", "CreatePicture", SelectionKind::Request),
    ("render", "FreePicture", SelectionKind::Request),
    ("render", "CreateCursor", SelectionKind::Request),
];

pub(crate) fn generate() -> Result<String, String> {
    let modules = load_modules()?;
    let mut gen = Gen::new(modules);
    gen.walk_selection()?;
    gen.emit()
}

fn load_modules() -> Result<Vec<Module>, String> {
    let mut modules = Vec::with_capacity(MODULE_FILES.len());
    for file in MODULE_FILES {
        let path = format!("{XCB_DIR}/{file}.xml");
        let xml = std::fs::read_to_string(&path).map_err(|err| format!("{path}: {err}"))?;
        let module = parse(&xml).map_err(|err| format!("{path}: {err}"))?;
        if module.header != *file {
            return Err(format!("{path}: header is {}, expected {file}", module.header));
        }
        modules.push(module);
    }
    Ok(modules)
}

struct Gen {
    modules: Vec<Module>,
    /// `(header, xml name)` of types that are emitted.
    included: HashSet<(String, String)>,
    seen: HashSet<(String, String)>,
}

impl Gen {
    fn new(modules: Vec<Module>) -> Self {
        Self { modules, included: HashSet::new(), seen: HashSet::new() }
    }

    fn module(&self, header: &str) -> Result<&Module, String> {
        self.modules
            .iter()
            .find(|module| module.header == header)
            .ok_or_else(|| format!("module {header} is not loaded"))
    }

    fn walk_selection(&mut self) -> Result<(), String> {
        for (header, name, kind) in SELECTION {
            self.walk_root(header, name, *kind)?;
        }
        Ok(())
    }

    fn walk_root(&mut self, header: &str, name: &str, kind: SelectionKind) -> Result<(), String> {
        let want = match kind {
            SelectionKind::Request => Want::Request,
            SelectionKind::Event => Want::Event,
            SelectionKind::Decl => Want::Type,
        };
        let (found, _) = self.resolve_owned(header, name, want)?;
        if found != header {
            return Err(format!(
                "{header}.{name} is not in that module (resolved through {found})"
            ));
        }
        self.walk(header, name, want)
    }

    fn walk(&mut self, from: &str, name: &str, want: Want) -> Result<(), String> {
        if want == Want::Type && primitive(name).is_some() {
            return Ok(());
        }
        let (header, item) = self.resolve_owned(from, name, want)?;
        let item_name = item_name(&item).to_string();
        if !self.seen.insert((header.clone(), item_name.clone())) {
            return Ok(());
        }
        match item {
            Item::Request(request) => {
                self.walk_fields(&header, &request.fields)?;
                if let Some(reply) = &request.reply {
                    self.walk_fields(&header, reply)?;
                }
            }
            Item::Event(event) => self.walk_fields(&header, &event.fields)?,
            Item::Error(error) => self.walk_fields(&header, &error.fields)?,
            Item::XidType(_) => {
                self.included.insert((header, item_name));
            }
            Item::XidUnion { types, .. } => {
                self.included.insert((header.clone(), item_name));
                for type_name in &types {
                    self.walk(&header, type_name, Want::Type)?;
                }
            }
            Item::Typedef { old_name, .. } => {
                self.included.insert((header.clone(), item_name));
                self.walk(&header, &old_name, Want::Type)?;
            }
            Item::Enum { .. } => {
                self.included.insert((header, item_name));
            }
            Item::Struct { fields, .. } | Item::Union { fields, .. } => {
                self.included.insert((header.clone(), item_name));
                self.walk_fields(&header, &fields)?;
            }
        }
        Ok(())
    }

    fn walk_fields(&mut self, from: &str, fields: &[Field]) -> Result<(), String> {
        for field in fields {
            self.walk_field(from, field)?;
        }
        Ok(())
    }

    fn walk_field(&mut self, from: &str, field: &Field) -> Result<(), String> {
        match field {
            Field::Pad(_) => Ok(()),
            Field::Field(named) | Field::Expr { field: named, .. } => self.walk_named(from, named),
            Field::List { field, length } => {
                self.walk_named(from, field)?;
                if let Some(expr) = length {
                    self.walk_expr(from, expr)?;
                }
                Ok(())
            }
            Field::Switch { expr, cases, .. } => {
                self.walk_expr(from, expr)?;
                for case in cases {
                    for expr in &case.exprs {
                        self.walk_expr(from, expr)?;
                    }
                    self.walk_fields(from, &case.fields)?;
                }
                Ok(())
            }
        }
    }

    fn walk_named(&mut self, from: &str, named: &Named) -> Result<(), String> {
        self.walk(from, &named.type_name, Want::Type)?;
        for enum_name in [&named.enum_name, &named.alt_enum, &named.mask]
            .into_iter()
            .flatten()
        {
            self.walk(from, enum_name, Want::Type)?;
        }
        Ok(())
    }

    fn walk_expr(&mut self, from: &str, expr: &Expr) -> Result<(), String> {
        match expr {
            Expr::FieldRef(_) | Expr::Value(_) => Ok(()),
            Expr::EnumRef { enum_name, .. } => self.walk(from, enum_name, Want::Type),
            Expr::Op { left, right, .. } => {
                self.walk_expr(from, left)?;
                self.walk_expr(from, right)
            }
        }
    }

    fn resolve<'a>(
        &'a self,
        from: &str,
        name: &str,
        want: Want,
    ) -> Result<(&'a Module, &'a Item), String> {
        let mut seen = HashSet::new();
        self.resolve_in(from, name, want, &mut seen)
            .ok_or_else(|| format!("unknown {} {name} referenced from {from}", want.label()))
    }

    fn resolve_in<'a>(
        &'a self,
        header: &str,
        name: &str,
        want: Want,
        seen: &mut HashSet<String>,
    ) -> Option<(&'a Module, &'a Item)> {
        if !seen.insert(header.to_string()) {
            return None;
        }
        let module = self.modules.iter().find(|module| module.header == header)?;
        if let Some(item) = module
            .items
            .iter()
            .find(|item| item_name(item) == name && want.matches(item))
        {
            return Some((module, item));
        }
        for import in &module.imports {
            if let Some(found) = self.resolve_in(import, name, want, seen) {
                return Some(found);
            }
        }
        None
    }

    fn resolve_owned(&self, from: &str, name: &str, want: Want) -> Result<(String, Item), String> {
        let (module, item) = self.resolve(from, name, want)?;
        Ok((module.header.clone(), item.clone()))
    }

    fn emit(&self) -> Result<String, String> {
        let mut out = String::new();
        out.push_str(
            "\
//! Generated from the installed xcb-proto descriptions. Do not edit.
//!
//! Rebuild with `cargo test regenerate_x11_generated -- --ignored`.
//!
//! `serialize` and `parse` speak the machine's native byte order. Alignment is
//! measured from the start of the reader or writer, which is the start of the
//! value being converted.

#![allow(non_upper_case_globals)]

use super::wire::{Reader, WireError, Writer};
",
        );
        let mut rust_names: HashMap<String, String> = HashMap::new();
        for file in MODULE_FILES {
            let module = self.module(file)?;
            for item in &module.items {
                if matches!(item, Item::Request(_) | Item::Event(_) | Item::Error(_)) {
                    continue;
                }
                let xml_name = item_name(item);
                if !self
                    .included
                    .contains(&(module.header.clone(), xml_name.to_string()))
                {
                    continue;
                }
                if self.merged_away(&module.header, xml_name)? {
                    continue;
                }
                let rust_name = type_name(xml_name);
                if let Some(previous) =
                    rust_names.insert(rust_name.clone(), format!("{}.{xml_name}", module.header))
                {
                    return Err(format!(
                        "rust type {rust_name} would be generated for both {previous} and {}.{}",
                        module.header, xml_name
                    ));
                }
                match item {
                    Item::XidType(_) => self.emit_xid(&mut out, &module.header, xml_name, &[])?,
                    Item::XidUnion { types, .. } => {
                        self.emit_xid(&mut out, &module.header, xml_name, types)?
                    }
                    Item::Typedef { old_name, new_name } => {
                        let target = self.alias_target(&module.header, old_name)?;
                        push(
                            &mut out,
                            0,
                            &format!("pub(crate) type {} = {target};", type_name(new_name)),
                        );
                        out.push('\n');
                    }
                    Item::Enum { name, items } => self.emit_enum(&mut out, name, items)?,
                    Item::Struct { name, fields } => {
                        self.emit_struct(&mut out, &module.header, name, fields)?
                    }
                    Item::Union { name, fields } => {
                        self.emit_union(&mut out, &module.header, name, fields)?
                    }
                    Item::Request(_) | Item::Event(_) | Item::Error(_) => {
                        return Err(format!(
                            "{}.{xml_name} is in the type closure but is not a type",
                            module.header
                        ));
                    }
                }
            }
        }
        self.emit_requests(&mut out, &mut rust_names)?;
        self.emit_events(&mut out, &mut rust_names)?;
        self.emit_errors(&mut out, &mut rust_names)?;
        Ok(out)
    }

    /// A value enum whose rust name matches an included xid in the same module
    /// contributes its constants to that xid.
    fn merged_away(&self, header: &str, xml_name: &str) -> Result<bool, String> {
        let Item::Enum { items, .. } = self.find_item(header, xml_name)? else {
            return Ok(false);
        };
        let rust_name = type_name(xml_name);
        let module = self.module(header)?;
        let xid = module.items.iter().any(|item| match item {
            Item::XidType(name) | Item::XidUnion { name, .. } => {
                type_name(name) == rust_name
                    && self.included.contains(&(header.to_string(), name.clone()))
            }
            _ => false,
        });
        if !xid {
            return Ok(false);
        }
        if items
            .iter()
            .any(|item| matches!(item.value, EnumValue::Bit(_)))
        {
            return Err(format!("{header}.{xml_name} is a bitflags enum and collides with an xid"));
        }
        Ok(true)
    }

    fn emit_xid(
        &self,
        out: &mut String,
        header: &str,
        xml_name: &str,
        members: &[String],
    ) -> Result<(), String> {
        let rust_name = type_name(xml_name);
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, &format!("pub(crate) struct {rust_name}(pub u32);"));
        out.push('\n');
        push(out, 0, &format!("impl {rust_name} {{"));
        push(out, 1, "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {");
        push(out, 2, "writer.u32(self.0);");
        push(out, 1, "}");
        out.push('\n');
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, "Ok(Self(reader.get_u32()?))");
        push(out, 1, "}");
        if let Some(items) = self.merged_items(header, &rust_name)? {
            for item in items {
                let const_name = const_ident(&item.name)?;
                let EnumValue::Value(value) = item.value else {
                    return Err(format!("{header}.{xml_name} mixed a bit into a value enum"));
                };
                push(out, 1, &format!("pub(crate) const {const_name}: Self = Self({value});"));
            }
        }
        push(out, 0, "}");
        out.push('\n');
        for member in members {
            let member_rust = self.xid_rust_name(header, member)?;
            push(out, 0, &format!("impl From<{member_rust}> for {rust_name} {{"));
            push(out, 1, &format!("fn from(value: {member_rust}) -> Self {{"));
            push(out, 2, "Self(value.0)");
            push(out, 1, "}");
            push(out, 0, "}");
            out.push('\n');
        }
        Ok(())
    }

    fn merged_items(&self, header: &str, rust_name: &str) -> Result<Option<Vec<EnumItem>>, String> {
        let module = self.module(header)?;
        for item in &module.items {
            let Item::Enum { name, items } = item else {
                continue;
            };
            if type_name(name) != rust_name {
                continue;
            }
            if !self.included.contains(&(header.to_string(), name.clone())) {
                continue;
            }
            return Ok(Some(items.clone()));
        }
        Ok(None)
    }

    fn xid_rust_name(&self, from: &str, name: &str) -> Result<String, String> {
        let (header, item) = self.resolve_owned(from, name, Want::Type)?;
        match item {
            Item::XidType(name) | Item::XidUnion { name, .. } => Ok(type_name(&name)),
            _ => Err(format!("{header}.{name} is not an xid")),
        }
    }

    fn alias_target(&self, from: &str, name: &str) -> Result<String, String> {
        Ok(self.classify(from, name, 0)?.rust_name())
    }

    fn emit_enum(
        &self,
        out: &mut String,
        xml_name: &str,
        items: &[EnumItem],
    ) -> Result<(), String> {
        let rust_name = type_name(xml_name);
        let bits = items
            .iter()
            .any(|item| matches!(item.value, EnumValue::Bit(_)));
        if bits {
            push(out, 0, "bitflags::bitflags! {");
            push(out, 1, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
            push(out, 1, &format!("pub(crate) struct {rust_name}: u32 {{"));
            for item in items {
                let const_name = const_ident(&item.name)?;
                let value = match item.value {
                    EnumValue::Bit(bit) if bit < 32 => format!("1u32 << {bit}"),
                    EnumValue::Bit(bit) => {
                        return Err(format!(
                            "{xml_name}.{const_name} has bit {bit}, which does not fit u32"
                        ));
                    }
                    EnumValue::Value(value) => format!("{value}u32"),
                };
                push(out, 2, &format!("const {const_name} = {value};"));
            }
            push(out, 1, "}");
            push(out, 0, "}");
            out.push('\n');
            return Ok(());
        }
        if items.is_empty() {
            return Err(format!("{xml_name} has no items"));
        }
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, &format!("pub(crate) struct {rust_name}(pub u32);"));
        out.push('\n');
        push(out, 0, &format!("impl {rust_name} {{"));
        for item in items {
            let const_name = const_ident(&item.name)?;
            let EnumValue::Value(value) = item.value else {
                return Err(format!("{xml_name} mixed a bit into a value enum"));
            };
            push(out, 1, &format!("pub(crate) const {const_name}: Self = Self({value});"));
        }
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn emit_struct(
        &self,
        out: &mut String,
        header: &str,
        xml_name: &str,
        fields: &[Field],
    ) -> Result<(), String> {
        let parts = self.lower_parts(header, xml_name, fields)?;
        let mut stack = Vec::new();
        let copy = self.parts_are_copy(header, &parts, &mut stack)?;
        let rust_name = type_name(xml_name);
        self.emit_struct_body(out, header, &rust_name, &parts, copy)
    }

    fn emit_struct_body(
        &self,
        out: &mut String,
        header: &str,
        rust_name: &str,
        parts: &[Part],
        copy: bool,
    ) -> Result<(), String> {
        let derives = if copy {
            "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]"
        } else {
            "#[derive(Clone, Debug, PartialEq, Eq, Hash)]"
        };
        push(out, 0, derives);
        push(out, 0, &format!("pub(crate) struct {rust_name} {{"));
        for part in parts {
            match part {
                Part::Pad(_) => {}
                Part::Field(field) | Part::List { field, .. } => {
                    let ty = self.field_rust_type(field, part)?;
                    push(out, 1, &format!("pub(crate) {}: {ty},", field.rust_name));
                }
            }
        }
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, &format!("impl {rust_name} {{"));
        push(out, 1, "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {");
        let scalars = scalar_fields(parts);
        for part in parts {
            match part {
                Part::Pad(Pad::Bytes(count)) => {
                    push(out, 2, &format!("writer.pad({count});"));
                }
                Part::Pad(Pad::Align(count)) => {
                    push(out, 2, &format!("writer.align({count});"));
                }
                Part::Field(field) => {
                    let value = format!("self.{}", field.rust_name);
                    push(out, 2, &write_value(&field.wire, &value));
                }
                Part::List { field, len } => {
                    if let ListLen::Dynamic(expr) = len {
                        let expected = expr_usize(expr, header, &scalars, "self.", self)?;
                        push(
                            out,
                            2,
                            &format!(
                                "debug_assert_eq!(self.{}.len(), {expected});",
                                field.rust_name
                            ),
                        );
                    }
                    if is_byte_blob(&field.wire) {
                        push(out, 2, &format!("writer.bytes(&self.{});", field.rust_name));
                    } else {
                        push(out, 2, &format!("for item in &self.{} {{", field.rust_name));
                        let value = list_item_expr(&field.wire);
                        push(out, 3, &write_value(&field.wire, &value));
                        push(out, 2, "}");
                    }
                }
            }
        }
        push(out, 1, "}");
        out.push('\n');
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        let mut parsed: Vec<Encoded> = Vec::new();
        for part in parts {
            match part {
                Part::Pad(Pad::Bytes(count)) => {
                    push(out, 2, &format!("reader.skip({count})?;"));
                }
                Part::Pad(Pad::Align(count)) => {
                    push(out, 2, &format!("reader.align({count})?;"));
                }
                Part::Field(field) => {
                    let loaded = read_value(&field.wire, &field.rust_ty);
                    push(out, 2, &format!("let {} = {loaded};", field.rust_name));
                    parsed.push(field.clone());
                }
                Part::List { field, len } => {
                    let count_expr = match len {
                        ListLen::Constant(count) => format!("{count}usize"),
                        ListLen::Dynamic(expr) => expr_usize(expr, header, &parsed, "", self)?,
                    };
                    let count_name = format!("{}_count", strip_raw(&field.rust_name));
                    push(out, 2, &format!("let {count_name} = {count_expr};"));
                    if is_byte_blob(&field.wire) {
                        match len {
                            ListLen::Constant(count) => {
                                push(
                                    out,
                                    2,
                                    &format!("let mut {} = [0u8; {count}];", field.rust_name),
                                );
                                push(
                                    out,
                                    2,
                                    &format!(
                                        "{}.copy_from_slice(reader.bytes({count_name})?);",
                                        field.rust_name
                                    ),
                                );
                            }
                            ListLen::Dynamic(_) => {
                                push(
                                    out,
                                    2,
                                    &format!(
                                        "let {} = reader.bytes({count_name})?.to_vec();",
                                        field.rust_name
                                    ),
                                );
                            }
                        }
                    } else {
                        push(
                            out,
                            2,
                            &format!(
                                "let mut {} = Vec::with_capacity({count_name});",
                                field.rust_name
                            ),
                        );
                        push(out, 2, &format!("for _ in 0..{count_name} {{"));
                        let loaded = read_value(&field.wire, &field.rust_ty);
                        push(out, 3, &format!("{}.push({loaded});", field.rust_name));
                        push(out, 2, "}");
                        if let ListLen::Constant(count) = len {
                            let ty = field.rust_ty.clone();
                            push(
                                out,
                                2,
                                &format!(
                                    "let {}: [{ty}; {count}] = {}.try_into().expect(\"fixed-length list\");",
                                    field.rust_name, field.rust_name
                                ),
                            );
                        }
                    }
                }
            }
        }
        push(out, 2, "Ok(Self {");
        for part in parts {
            if let Part::Field(field) | Part::List { field, .. } = part {
                push(out, 3, &format!("{},", field.rust_name));
            }
        }
        push(out, 2, "})");
        push(out, 1, "}");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn field_rust_type(&self, field: &Encoded, part: &Part) -> Result<String, String> {
        match part {
            Part::List { len: ListLen::Constant(count), .. } => {
                Ok(format!("[{}; {count}]", field.rust_ty))
            }
            Part::List { len: ListLen::Dynamic(_), .. } => Ok(format!("Vec<{}>", field.rust_ty)),
            Part::Field(_) => Ok(field.rust_ty.clone()),
            Part::Pad(_) => Err("pad has no rust type".into()),
        }
    }

    fn emit_union(
        &self,
        out: &mut String,
        header: &str,
        xml_name: &str,
        fields: &[Field],
    ) -> Result<(), String> {
        let mut members = Vec::new();
        let mut size = 0usize;
        for field in fields {
            let member = self.union_member(header, xml_name, field)?;
            size = size.max(member.bytes);
            members.push(member);
        }
        if size == 0 {
            return Err(format!("{xml_name} is an empty union"));
        }
        let rust_name = type_name(xml_name);
        push(out, 0, "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]");
        push(out, 0, &format!("pub(crate) struct {rust_name} {{"));
        push(out, 1, &format!("bytes: [u8; {size}],"));
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, &format!("impl {rust_name} {{"));
        push(out, 1, "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) {");
        push(out, 2, "writer.bytes(&self.bytes);");
        push(out, 1, "}");
        out.push('\n');
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, &format!("let mut bytes = [0; {size}];"));
        push(out, 2, &format!("bytes.copy_from_slice(reader.bytes({size})?);"));
        push(out, 2, "Ok(Self { bytes })");
        push(out, 1, "}");
        for member in &members {
            self.emit_union_accessors(out, member, size)?;
        }
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn union_member(
        &self,
        header: &str,
        owner: &str,
        field: &Field,
    ) -> Result<UnionMember, String> {
        match field {
            Field::List { field, length: Some(Expr::Value(count)) } => {
                let encoded = self.encode_named(header, field)?;
                let prim = encoded
                    .wire
                    .as_prim()
                    .ok_or_else(|| format!("{owner}.{} is not a primitive list", field.name))?;
                let count = usize::try_from(*count)
                    .map_err(|_| format!("{owner}.{} length does not fit usize", field.name))?;
                let bytes = count
                    .checked_mul(prim.size())
                    .ok_or_else(|| format!("{owner}.{} is too large", field.name))?;
                Ok(UnionMember { name: encoded.rust_name, prim, count, bytes })
            }
            Field::Field(named) => {
                let encoded = self.encode_named(header, named)?;
                let prim = encoded
                    .wire
                    .as_prim()
                    .ok_or_else(|| format!("{owner}.{} is not a primitive field", named.name))?;
                Ok(UnionMember { name: encoded.rust_name, prim, count: 1, bytes: prim.size() })
            }
            Field::Pad(Pad::Bytes(count)) => {
                let count = usize::try_from(*count)
                    .map_err(|_| format!("{owner} pad does not fit usize"))?;
                Ok(UnionMember { name: String::new(), prim: Prim::U8, count: 0, bytes: count })
            }
            Field::Pad(Pad::Align(_)) => {
                Ok(UnionMember { name: String::new(), prim: Prim::U8, count: 0, bytes: 0 })
            }
            Field::List { field, .. } => {
                Err(format!("{owner}.{} needs a constant length", field.name))
            }
            Field::Expr { field, .. } => Err(format!(
                "phase 3 emits plain structs only: {owner} contains exprfield {}",
                field.name
            )),
            Field::Switch { name, .. } => {
                Err(format!("phase 3 emits plain structs only: {owner} contains switch {name}"))
            }
        }
    }

    fn emit_union_accessors(
        &self,
        out: &mut String,
        member: &UnionMember,
        union_size: usize,
    ) -> Result<(), String> {
        if member.name.is_empty() {
            return Ok(());
        }
        let rust = member.prim.rust();
        let width = member.prim.size();
        out.push('\n');
        if member.count == 1 {
            push(out, 1, &format!("pub(crate) fn {}(&self) -> {rust} {{", member.name));
            push(out, 2, &member.prim.read_expr("self.bytes", 0));
            push(out, 1, "}");
            out.push('\n');
            push(
                out,
                1,
                &format!(
                    "pub(crate) fn set_{}(&mut self, value: {rust}) {{",
                    strip_raw(&member.name)
                ),
            );
            push(out, 2, &format!("self.bytes = [0; {union_size}];"));
            push(out, 2, &member.prim.write_at("self.bytes", "value", 0));
            push(out, 1, "}");
            return Ok(());
        }
        push(
            out,
            1,
            &format!("pub(crate) fn {}(&self) -> [{rust}; {}] {{", member.name, member.count),
        );
        push(out, 2, &format!("let mut out = [0; {}];", member.count));
        push(out, 2, "for (index, slot) in out.iter_mut().enumerate() {");
        push(out, 3, &format!("let start = index * {width};"));
        push(out, 3, &member.prim.read_slot());
        push(out, 2, "}");
        push(out, 2, "out");
        push(out, 1, "}");
        out.push('\n');
        push(
            out,
            1,
            &format!(
                "pub(crate) fn set_{}(&mut self, value: [{rust}; {}]) {{",
                strip_raw(&member.name),
                member.count
            ),
        );
        push(out, 2, &format!("self.bytes = [0; {union_size}];"));
        push(out, 2, "for (index, slot) in value.into_iter().enumerate() {");
        push(out, 3, &format!("let start = index * {width};"));
        push(
            out,
            3,
            &format!("self.bytes[start..start + {width}].copy_from_slice(&slot.to_ne_bytes());"),
        );
        push(out, 2, "}");
        push(out, 1, "}");
        Ok(())
    }

    fn lower_parts(
        &self,
        header: &str,
        owner: &str,
        fields: &[Field],
    ) -> Result<Vec<Part>, String> {
        let mut parts = Vec::new();
        for field in fields {
            match field {
                Field::Pad(pad) => parts.push(Part::Pad(*pad)),
                Field::Field(named) => parts.push(Part::Field(self.encode_named(header, named)?)),
                Field::List { field, length } => {
                    let encoded = self.encode_named(header, field)?;
                    let len = match length {
                        None => {
                            return Err(format!("struct {owner} list {} has no length", field.name))
                        }
                        Some(Expr::Value(count)) => {
                            let count = usize::try_from(*count).map_err(|_| {
                                format!("{owner}.{} length does not fit usize", field.name)
                            })?;
                            ListLen::Constant(count)
                        }
                        Some(expr) => ListLen::Dynamic(expr.clone()),
                    };
                    parts.push(Part::List { field: encoded, len });
                }
                Field::Expr { field, .. } => {
                    return Err(format!(
                        "phase 3 emits plain structs only: {owner} contains exprfield {}",
                        field.name
                    ));
                }
                Field::Switch { name, .. } => {
                    return Err(format!(
                        "phase 3 emits plain structs only: {owner} contains switch {name}"
                    ));
                }
            }
        }
        Ok(parts)
    }

    fn encode_named(&self, from: &str, named: &Named) -> Result<Encoded, String> {
        if let (Some(mask), Some(enum_name)) = (&named.mask, &named.enum_name) {
            if mask != enum_name {
                return Err(format!(
                    "field {} has both mask {mask} and enum {enum_name}",
                    named.name
                ));
            }
        }
        let class = self.classify(from, &named.type_name, 0)?;
        let overlaid = named.mask.as_deref().or(named.enum_name.as_deref());
        let info = if let Some(enum_name) = overlaid {
            let width = class.prim_width().ok_or_else(|| {
                format!(
                    "field {} types {} with enum {enum_name}, which needs a primitive width",
                    named.name, named.type_name
                )
            })?;
            let enum_class = self.classify(from, enum_name, 0)?;
            match enum_class {
                Class::Bits { rust, .. } => {
                    if !matches!(width, Prim::U8 | Prim::U16 | Prim::U32) {
                        return Err(format!(
                            "field {} uses bitflags {rust} on {}",
                            named.name, named.type_name
                        ));
                    }
                    WireInfo { rust, wire: Wire::Bits { width }, nested: None }
                }
                Class::Value { rust, .. } => {
                    WireInfo { rust, wire: Wire::ValueEnum { width }, nested: None }
                }
                _ => {
                    return Err(format!(
                        "field {} attribute {enum_name} is not an enum",
                        named.name
                    ))
                }
            }
        } else {
            class.to_wire()?
        };
        Ok(Encoded {
            xml_name: named.name.clone(),
            rust_name: ident(&named.name)?,
            rust_ty: info.rust,
            wire: info.wire,
            nested: info.nested,
        })
    }

    fn classify(&self, from: &str, name: &str, depth: usize) -> Result<Class, String> {
        if depth > 16 {
            return Err(format!("typedef cycle at {name}"));
        }
        if let Some(prim) = primitive(name) {
            return Ok(Class::Prim(prim));
        }
        let (header, item) = self.resolve_owned(from, name, Want::Type)?;
        match item {
            Item::Typedef { old_name, new_name } => {
                let inner = self.classify(&header, &old_name, depth + 1)?;
                Ok(Class::Alias { rust: type_name(&new_name), inner: Box::new(inner) })
            }
            Item::XidType(name) | Item::XidUnion { name, .. } => {
                Ok(Class::Xid { rust: type_name(&name) })
            }
            Item::Struct { name, .. } => {
                Ok(Class::Struct { rust: type_name(&name), header, xml: name })
            }
            Item::Union { name, .. } => Ok(Class::Union { rust: type_name(&name) }),
            Item::Enum { name, items } => {
                let rust = type_name(&name);
                if items
                    .iter()
                    .any(|item| matches!(item.value, EnumValue::Bit(_)))
                {
                    Ok(Class::Bits { rust })
                } else {
                    Ok(Class::Value { rust })
                }
            }
            other => Err(format!("{header}.{} is not a type", item_name(&other))),
        }
    }

    fn parts_are_copy(
        &self,
        header: &str,
        parts: &[Part],
        stack: &mut Vec<(String, String)>,
    ) -> Result<bool, String> {
        for part in parts {
            match part {
                Part::Pad(_) => {}
                Part::List { len: ListLen::Dynamic(_), .. } => return Ok(false),
                Part::Field(field) | Part::List { field, len: ListLen::Constant(_), .. } => {
                    if !self.field_is_copy(header, field, stack)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    fn field_is_copy(
        &self,
        _header: &str,
        field: &Encoded,
        stack: &mut Vec<(String, String)>,
    ) -> Result<bool, String> {
        match &field.wire {
            Wire::Prim(_)
            | Wire::Xid
            | Wire::ValueEnum { .. }
            | Wire::Bits { .. }
            | Wire::Union => Ok(true),
            Wire::Struct => {
                let (header, xml) = field
                    .nested
                    .clone()
                    .ok_or_else(|| format!("struct field {} has no nested type", field.xml_name))?;
                if stack
                    .iter()
                    .any(|(seen_header, seen_xml)| seen_header == &header && seen_xml == &xml)
                {
                    return Err(format!("struct cycle at {header}.{xml}"));
                }
                stack.push((header.clone(), xml.clone()));
                let fields = self.struct_fields(&header, &xml)?;
                let parts = self.lower_parts(&header, &xml, &fields)?;
                let copy = self.parts_are_copy(&header, &parts, stack)?;
                stack.pop();
                Ok(copy)
            }
        }
    }

    fn struct_fields(&self, header: &str, xml: &str) -> Result<Vec<Field>, String> {
        match self.find_item(header, xml)? {
            Item::Struct { fields, .. } => Ok(fields.clone()),
            _ => Err(format!("{header}.{xml} is not a struct")),
        }
    }

    fn find_item(&self, header: &str, xml: &str) -> Result<&Item, String> {
        let module = self.module(header)?;
        module
            .items
            .iter()
            .find(|item| {
                item_name(item) == xml
                    && !matches!(item, Item::Request(_) | Item::Event(_) | Item::Error(_))
            })
            .ok_or_else(|| format!("{header}.{xml} is not in that module"))
    }

    /// Emits the request trait, request structs, value-list auxiliaries, and replies.
    fn emit_requests(
        &self,
        out: &mut String,
        rust_names: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        self.emit_request_preamble(out)?;
        for (header, name, kind) in SELECTION {
            if *kind != SelectionKind::Request {
                continue;
            }
            self.emit_one_request(out, rust_names, header, name)?;
        }
        Ok(())
    }

    fn emit_request_preamble(&self, out: &mut String) -> Result<(), String> {
        let mut variants = Vec::new();
        for (header, _, kind) in SELECTION {
            if *kind != SelectionKind::Request {
                continue;
            }
            let Some(name) = &self.module(header)?.extension_name else {
                continue;
            };
            let variant = type_name(name);
            if !variants.contains(&variant) {
                variants.push(variant);
            }
        }
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, "pub(crate) enum ExtensionName {");
        for variant in &variants {
            push(out, 1, &format!("{variant},"));
        }
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]");
        push(out, 0, "pub(crate) struct NoReply;");
        out.push('\n');
        push(out, 0, "pub(crate) trait Request {");
        push(out, 1, "const NAME: &'static str;");
        push(out, 1, "const EXTENSION: Option<ExtensionName>;");
        push(out, 1, "/// Core opcode, or the extension's minor opcode.");
        push(out, 1, "const OPCODE: u8;");
        push(out, 1, "type Reply;");
        push(
            out,
            1,
            "fn serialize(&self, writer: &mut Writer<'_>, major_opcode: u8) -> Result<(), WireError>;",
        );
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "pub(crate) trait XReply: Sized {");
        push(out, 1, "fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError>;");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn emit_one_request(
        &self,
        out: &mut String,
        rust_names: &mut HashMap<String, String>,
        header: &str,
        name: &str,
    ) -> Result<(), String> {
        let module = self.module(header)?;
        let request = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Request(request) if request.name == name => Some(request),
                _ => None,
            })
            .ok_or_else(|| format!("{header}.{name} request is not in that module"))?;
        let extension = match &module.extension_name {
            Some(extension) => Some(type_name(extension)),
            None => {
                if header != "xproto" {
                    return Err(format!("{header} has no extension name"));
                }
                None
            }
        };
        let rust_name = match &extension {
            Some(prefix) => format!("{prefix}{}", type_name(name)),
            None => type_name(name),
        };
        let aux_type = format!("{rust_name}Aux");
        let reply_name = format!("{rust_name}Reply");
        let message = self.lower_message(
            header,
            &format!("{header}.{name}"),
            &request.fields,
            extension.is_some(),
            true,
            &aux_type,
        )?;
        let reply = match &request.reply {
            Some(fields) => Some(self.lower_message(
                header,
                &format!("{header}.{name} reply"),
                fields,
                false,
                false,
                &aux_type,
            )?),
            None => None,
        };
        claim(rust_names, &rust_name, format!("{header}.{name}"))?;
        if message.aux.is_some() {
            claim(rust_names, &aux_type, format!("{header}.{name} aux"))?;
            self.emit_aux(out, &aux_type, message.aux.as_ref().unwrap())?;
        }
        emit_struct_type(
            out,
            &rust_name,
            message.lifetime,
            &message.fields,
            message_is_copy(&message),
        );
        out.push('\n');
        if let Some(reply) = &reply {
            claim(rust_names, &reply_name, format!("{header}.{name} reply"))?;
            self.emit_reply_type(out, header, &reply_name, reply)?;
        }
        self.emit_request_impl(
            out,
            &rust_name,
            extension.as_deref(),
            request.opcode,
            message.lifetime,
            if reply.is_some() { &reply_name } else { "NoReply" },
            &message,
        )?;
        Ok(())
    }

    fn emit_aux(&self, out: &mut String, type_name: &str, aux: &AuxDef) -> Result<(), String> {
        push(out, 0, derives(aux.copy, true));
        push(out, 0, &format!("pub(crate) struct {type_name} {{"));
        for case in &aux.cases {
            push(
                out,
                1,
                &format!("pub(crate) {}: Option<{}>,", case.field.rust_name, case.field.rust_ty),
            );
        }
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, &format!("impl {type_name} {{"));
        for case in &aux.cases {
            push(
                out,
                1,
                &format!(
                    "pub(crate) fn {}(mut self, value: {}) -> Self {{",
                    case.field.rust_name, case.field.rust_ty
                ),
            );
            push(out, 2, &format!("self.{} = Some(value);", case.field.rust_name));
            push(out, 2, "self");
            push(out, 1, "}");
            out.push('\n');
        }
        push(out, 1, &format!("pub(crate) fn mask(&self) -> {} {{", aux.mask_ty));
        if aux.cases.is_empty() {
            push(out, 2, "let _ = self;");
        }
        push(out, 2, &format!("let mut mask = {}::empty();", aux.mask_ty));
        for case in &aux.cases {
            push(out, 2, &format!("if self.{}.is_some() {{", case.field.rust_name));
            push(out, 3, &format!("mask |= {};", case.flag));
            push(out, 2, "}");
        }
        push(out, 2, "mask");
        push(out, 1, "}");
        out.push('\n');
        push(out, 1, "pub(crate) fn write(&self, writer: &mut Writer<'_>) {");
        if aux.cases.is_empty() {
            push(out, 2, "let _ = self;");
            push(out, 2, "let _ = writer;");
        }
        for case in &aux.cases {
            push(out, 2, &format!("if let Some(value) = &self.{} {{", case.field.rust_name));
            for op in &case.ops {
                match op {
                    CaseOp::Pad(pad) => emit_pad(out, 3, *pad, true),
                    CaseOp::Field => emit_aux_field_write(out, 3, &case.field),
                }
            }
            push(out, 2, "}");
        }
        push(out, 1, "}");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn emit_reply_type(
        &self,
        out: &mut String,
        header: &str,
        name: &str,
        message: &Message,
    ) -> Result<(), String> {
        push(out, 0, derives(message_is_copy(message), false));
        push(out, 0, &format!("pub(crate) struct {name} {{"));
        push(out, 1, "pub(crate) sequence: u16,");
        for field in &message.fields {
            push(out, 1, &format!("pub(crate) {}: {},", field.name, field.ty));
        }
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, &format!("impl {name} {{"));
        push(
            out,
            1,
            "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {",
        );
        emit_serialize_stmts(out, 2, message, SerMode::Reply);
        push(out, 1, "}");
        out.push('\n');
        self.emit_reply_parse(out, header, name, message)?;
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, &format!("impl XReply for {name} {{"));
        push(out, 1, "fn parse_reply(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, &format!("{name}::parse(reader)"));
        push(out, 1, "}");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn emit_reply_parse(
        &self,
        out: &mut String,
        header: &str,
        name: &str,
        message: &Message,
    ) -> Result<(), String> {
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, "let _response = reader.get_u8()?;");
        let mut parsed = Vec::new();
        match &message.header {
            HeaderByte::Field(field) => {
                let loaded = read_value(&field.wire, &field.rust_ty);
                push(out, 2, &format!("let {} = {loaded};", field.rust_name));
                parsed.push(field.clone());
            }
            HeaderByte::Zero => push(out, 2, "let _data = reader.get_u8()?;"),
        }
        push(out, 2, "let sequence = reader.get_u16()?;");
        push(out, 2, "let _length = reader.get_u32()?;");
        for op in &message.ops {
            match op {
                Op::Pad(pad) => emit_pad(out, 2, *pad, false),
                Op::Store(field) | Op::Len(field) => {
                    let loaded = read_value(&field.wire, &field.rust_ty);
                    push(out, 2, &format!("let {} = {loaded};", field.rust_name));
                    parsed.push(field.clone());
                }
                Op::List { field, count, .. } => {
                    self.emit_list_parse(out, header, field, count, &parsed)?;
                }
                Op::Mask { .. } | Op::Aux(_) => {
                    return Err(format!("{name} reply contains a value list"));
                }
            }
        }
        push(out, 2, "reader.finish_reply()?;");
        push(out, 2, "Ok(Self {");
        push(out, 3, "sequence,");
        for field in &message.fields {
            push(out, 3, &format!("{},", field.name));
        }
        push(out, 2, "})");
        push(out, 1, "}");
        Ok(())
    }

    fn emit_list_parse(
        &self,
        out: &mut String,
        header: &str,
        field: &Encoded,
        count: &ListCount,
        parsed: &[Encoded],
    ) -> Result<(), String> {
        let count_expr = match count {
            ListCount::Fixed(count) => format!("{count}usize"),
            ListCount::Expr(expr) => expr_usize(expr, header, parsed, "", self)?,
            ListCount::Rest => {
                return Err(format!("{header}.{} list has no length", field.xml_name));
            }
        };
        let count_name = format!("{}_count", strip_raw(&field.rust_name));
        push(out, 2, &format!("let {count_name} = {count_expr};"));
        if is_byte_blob(&field.wire) {
            match count {
                ListCount::Fixed(count) => {
                    push(out, 2, &format!("let mut {} = [0u8; {count}];", field.rust_name));
                    push(
                        out,
                        2,
                        &format!(
                            "{}.copy_from_slice(reader.bytes({count_name})?);",
                            field.rust_name
                        ),
                    );
                }
                ListCount::Expr(_) => {
                    push(
                        out,
                        2,
                        &format!("let {} = reader.bytes({count_name})?.to_vec();", field.rust_name),
                    );
                }
                ListCount::Rest => unreachable!("rest lists are rejected above"),
            }
        } else {
            push(
                out,
                2,
                &format!("let mut {} = Vec::with_capacity({count_name});", field.rust_name),
            );
            push(out, 2, &format!("for _ in 0..{count_name} {{"));
            let loaded = read_value(&field.wire, &field.rust_ty);
            push(out, 3, &format!("{}.push({loaded});", field.rust_name));
            push(out, 2, "}");
            if let ListCount::Fixed(count) = count {
                push(
                    out,
                    2,
                    &format!(
                        "let {}: [{}; {count}] = {}.try_into().expect(\"fixed-length list\");",
                        field.rust_name, field.rust_ty, field.rust_name
                    ),
                );
            }
        }
        Ok(())
    }

    fn emit_request_impl(
        &self,
        out: &mut String,
        name: &str,
        extension: Option<&str>,
        opcode: u8,
        lifetime: bool,
        reply: &str,
        message: &Message,
    ) -> Result<(), String> {
        if lifetime {
            push(out, 0, &format!("impl<'a> Request for {name}<'a> {{"));
        } else {
            push(out, 0, &format!("impl Request for {name} {{"));
        }
        push(out, 1, &format!("const NAME: &'static str = \"{name}\";"));
        match extension {
            Some(extension) => push(
                out,
                1,
                &format!(
                    "const EXTENSION: Option<ExtensionName> = Some(ExtensionName::{extension});"
                ),
            ),
            None => push(out, 1, "const EXTENSION: Option<ExtensionName> = None;"),
        }
        push(out, 1, &format!("const OPCODE: u8 = {opcode};"));
        push(out, 1, &format!("type Reply = {reply};"));
        let major = if extension.is_some() { "major_opcode" } else { "_major_opcode" };
        push(
            out,
            1,
            &format!(
                "fn serialize(&self, writer: &mut Writer<'_>, {major}: u8) -> Result<(), WireError> {{"
            ),
        );
        let mode = if extension.is_some() { SerMode::Extension } else { SerMode::Core };
        emit_serialize_stmts(out, 2, message, mode);
        push(out, 1, "}");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn lower_message(
        &self,
        header: &str,
        owner: &str,
        fields: &[Field],
        extension_request: bool,
        borrow_lists: bool,
        aux_type: &str,
    ) -> Result<Message, String> {
        let switch = self.prepare_switch(header, owner, fields, borrow_lists)?;
        let mut bare: HashMap<String, String> = HashMap::new();
        for field in fields {
            if let Field::List { field: named, length: Some(Expr::FieldRef(len_name)) } = field {
                let list_name = ident(&named.name)?;
                if bare.insert(len_name.clone(), list_name).is_some() {
                    return Err(format!("{owner}.{len_name} is the length of two lists"));
                }
            }
        }
        if let Some(info) = &switch {
            if bare.contains_key(&info.mask_xml) {
                return Err(format!(
                    "{owner}.{} is both a value mask and a list length",
                    info.mask_xml
                ));
            }
        }
        let mut index = 0;
        let header_byte = if extension_request {
            HeaderByte::Zero
        } else {
            match fields.first() {
                Some(Field::Pad(Pad::Bytes(1))) => {
                    index = 1;
                    HeaderByte::Zero
                }
                Some(Field::Field(named)) => {
                    let encoded = self.encode_named(header, named)?;
                    if is_one_byte(&encoded.wire) {
                        let masks_header = match &switch {
                            Some(info) => info.mask_xml == named.name,
                            None => false,
                        };
                        if bare.contains_key(&named.name) || masks_header {
                            return Err(format!(
                                "{owner}.{} cannot be both the header byte and a computed field",
                                named.name
                            ));
                        }
                        index = 1;
                        HeaderByte::Field(encoded)
                    } else {
                        HeaderByte::Zero
                    }
                }
                _ => HeaderByte::Zero,
            }
        };
        let mut rust_fields = Vec::new();
        let mut seen = HashSet::new();
        let mut ops = Vec::new();
        let mut lengths = Vec::new();
        let mut lifetime = false;
        if let HeaderByte::Field(field) = &header_byte {
            let copy = self.field_is_copy(header, field, &mut Vec::new())?;
            push_rust_field(
                &mut rust_fields,
                &mut seen,
                owner,
                field,
                field.rust_ty.clone(),
                copy,
            )?;
        }
        for field in &fields[index..] {
            match field {
                Field::Pad(pad) => ops.push(Op::Pad(*pad)),
                Field::Expr { field, .. } => {
                    return Err(format!("{owner} contains exprfield {}", field.name));
                }
                Field::Field(named) => {
                    let encoded = self.encode_named(header, named)?;
                    if let Some(info) = &switch {
                        if named.name == info.mask_xml {
                            if !seen.insert(encoded.rust_name.clone()) {
                                return Err(format!(
                                    "{owner} field {} is duplicated",
                                    encoded.rust_name
                                ));
                            }
                            ops.push(Op::Mask { aux: info.field_name.clone(), width: info.width });
                            continue;
                        }
                    }
                    if let Some(list) = bare.get(&named.name) {
                        let prim = unsigned_length(&encoded.wire).ok_or_else(|| {
                            format!("{owner}.{} length is not an unsigned integer", named.name)
                        })?;
                        if !seen.insert(encoded.rust_name.clone()) {
                            return Err(format!(
                                "{owner} field {} is duplicated",
                                encoded.rust_name
                            ));
                        }
                        lengths.push(LengthLocal {
                            local: encoded.rust_name.clone(),
                            ty: prim.rust().to_string(),
                            list: list.clone(),
                        });
                        ops.push(Op::Len(encoded));
                        continue;
                    }
                    let copy = self.field_is_copy(header, &encoded, &mut Vec::new())?;
                    let ty = encoded.rust_ty.clone();
                    push_rust_field(&mut rust_fields, &mut seen, owner, &encoded, ty, copy)?;
                    ops.push(Op::Store(encoded));
                }
                Field::List { field: named, length } => {
                    let encoded = self.encode_named(header, named)?;
                    let count = match length {
                        None => {
                            if !borrow_lists {
                                return Err(format!("{owner}.{} list has no length", named.name));
                            }
                            ListCount::Rest
                        }
                        Some(Expr::Value(count)) => {
                            let count = usize::try_from(*count).map_err(|_| {
                                format!("{owner}.{} length does not fit usize", named.name)
                            })?;
                            ListCount::Fixed(count)
                        }
                        Some(expr) => ListCount::Expr(expr.clone()),
                    };
                    let ty = list_rust_type(&encoded.rust_ty, &count, borrow_lists);
                    let copy = self.list_is_copy(header, &encoded, &count, borrow_lists)?;
                    if borrow_lists {
                        lifetime = true;
                    }
                    push_rust_field(&mut rust_fields, &mut seen, owner, &encoded, ty, copy)?;
                    ops.push(Op::List { field: encoded, count, borrow: borrow_lists });
                }
                Field::Switch { .. } => {
                    let info = switch
                        .as_ref()
                        .ok_or_else(|| format!("{owner} switch was not prepared"))?;
                    push_rust_field(
                        &mut rust_fields,
                        &mut seen,
                        owner,
                        &Encoded {
                            xml_name: info.field_name.clone(),
                            rust_name: info.field_name.clone(),
                            rust_ty: aux_type.to_string(),
                            wire: Wire::Struct,
                            nested: None,
                        },
                        aux_type.to_string(),
                        info.copy,
                    )?;
                    ops.push(Op::Aux(info.field_name.clone()));
                }
            }
        }
        for name in bare.keys() {
            let written = ops
                .iter()
                .any(|op| matches!(op, Op::Len(field) if field.xml_name == *name));
            if !written {
                return Err(format!("{owner} length field {name} was not written"));
            }
        }
        if switch.is_some() && !ops.iter().any(|op| matches!(op, Op::Mask { .. })) {
            return Err(format!("{owner} does not write its value mask"));
        }
        let mut scalars = Vec::new();
        if let HeaderByte::Field(field) = &header_byte {
            scalars.push(field.clone());
        }
        for op in &ops {
            if let Op::Store(field) = op {
                scalars.push(field.clone());
            }
        }
        let mut checks = Vec::new();
        for op in &ops {
            if let Op::List { field, count: ListCount::Expr(expr), .. } = op {
                if !matches!(expr, Expr::FieldRef(_)) {
                    let expected = expr_usize(expr, header, &scalars, "self.", self)?;
                    checks.push(format!(
                        "debug_assert_eq!(self.{}.len(), {expected});",
                        field.rust_name
                    ));
                }
            }
        }
        let aux = switch.map(|info| AuxDef {
            mask_ty: info.mask.rust_ty,
            cases: info.cases,
            copy: info.copy,
        });
        Ok(Message {
            header: header_byte,
            ops,
            fields: rust_fields,
            lengths,
            checks,
            aux,
            lifetime,
        })
    }

    fn prepare_switch(
        &self,
        header: &str,
        owner: &str,
        fields: &[Field],
        borrow_lists: bool,
    ) -> Result<Option<SwitchInfo>, String> {
        let mut found = None;
        for field in fields {
            let Field::Switch { name, expr, cases } = field else {
                continue;
            };
            if found.is_some() {
                return Err(format!("{owner} has more than one switch"));
            }
            if !borrow_lists {
                return Err(format!("{owner} contains switch {name}"));
            }
            let Expr::FieldRef(mask_xml) = expr else {
                return Err(format!("{owner} switch {name} expression is not a field"));
            };
            let named = fields
                .iter()
                .find_map(|item| match item {
                    Field::Field(named) if &named.name == mask_xml => Some(named),
                    _ => None,
                })
                .ok_or_else(|| {
                    format!("{owner} switch {name} references {mask_xml}, which is not a field")
                })?;
            let mask = self.encode_named(header, named)?;
            let Wire::Bits { width } = mask.wire else {
                return Err(format!("{owner}.{mask_xml} value mask is not bitflags"));
            };
            let cases = self.lower_cases(header, owner, cases)?;
            let mut stack = Vec::new();
            let mut copy = true;
            let mut names = HashSet::new();
            for case in &cases {
                let plain = strip_raw(&case.field.rust_name);
                if plain == "mask" || plain == "write" {
                    return Err(format!(
                        "{owner} aux field {} clashes with a method",
                        case.field.rust_name
                    ));
                }
                if !names.insert(case.field.rust_name.clone()) {
                    return Err(format!(
                        "{owner} aux field {} is duplicated",
                        case.field.rust_name
                    ));
                }
                if !self.field_is_copy(header, &case.field, &mut stack)? {
                    copy = false;
                }
            }
            found = Some(SwitchInfo {
                field_name: ident(name)?,
                mask_xml: mask_xml.clone(),
                mask,
                width,
                cases,
                copy,
            });
        }
        Ok(found)
    }

    fn lower_cases(
        &self,
        header: &str,
        owner: &str,
        cases: &[BitCase],
    ) -> Result<Vec<AuxCase>, String> {
        let mut lowered = Vec::new();
        let mut seen_bits = HashSet::new();
        for case in cases {
            let (bit, flag) = self.case_flag(header, owner, &case.exprs)?;
            if !seen_bits.insert(bit) {
                return Err(format!("{owner} repeats bit {bit}"));
            }
            let mut ops = Vec::new();
            let mut field = None;
            for item in &case.fields {
                match item {
                    Field::Pad(pad) => ops.push(CaseOp::Pad(*pad)),
                    Field::Field(named) => {
                        if field.is_some() {
                            return Err(format!("{owner} bitcase has more than one field"));
                        }
                        field = Some(self.encode_named(header, named)?);
                        ops.push(CaseOp::Field);
                    }
                    Field::List { field, .. } => {
                        return Err(format!("{owner} bitcase {} contains a list", field.name));
                    }
                    Field::Expr { field, .. } => {
                        return Err(format!("{owner} bitcase contains exprfield {}", field.name));
                    }
                    Field::Switch { name, .. } => {
                        return Err(format!("{owner} bitcase contains switch {name}"));
                    }
                }
            }
            let Some(field) = field else {
                return Err(format!("{owner} bitcase has no field"));
            };
            lowered.push(AuxCase { bit, flag, field, ops });
        }
        lowered.sort_by_key(|case| case.bit);
        Ok(lowered)
    }

    fn case_flag(&self, header: &str, owner: &str, exprs: &[Expr]) -> Result<(u8, String), String> {
        let mut found = None;
        for expr in exprs {
            let Expr::EnumRef { enum_name, item } = expr else {
                return Err(format!("{owner} bitcase expression is not an enumref"));
            };
            if found.is_some() {
                return Err(format!("{owner} bitcase has more than one enumref"));
            }
            found = Some(self.enum_bit(header, enum_name, item)?);
        }
        found.ok_or_else(|| format!("{owner} bitcase has no enumref"))
    }

    fn enum_bit(&self, header: &str, enum_name: &str, item: &str) -> Result<(u8, String), String> {
        let (found_header, found) = self.resolve_owned(header, enum_name, Want::Type)?;
        let Item::Enum { items, .. } = found else {
            return Err(format!("{found_header}.{enum_name} is not an enum"));
        };
        let enum_item = items
            .iter()
            .find(|candidate| candidate.name == item)
            .ok_or_else(|| format!("{enum_name}.{item} is not an enum item"))?;
        let EnumValue::Bit(bit) = enum_item.value else {
            return Err(format!("{enum_name}.{item} is not a bit"));
        };
        if bit > 31 {
            return Err(format!("{enum_name}.{item} bit {bit} does not fit a mask"));
        }
        Ok((bit, format!("{}::{}", type_name(enum_name), const_ident(item)?)))
    }

    fn list_is_copy(
        &self,
        header: &str,
        field: &Encoded,
        count: &ListCount,
        borrow: bool,
    ) -> Result<bool, String> {
        if borrow {
            return Ok(true);
        }
        match count {
            ListCount::Fixed(_) => self.field_is_copy(header, field, &mut Vec::new()),
            ListCount::Expr(_) | ListCount::Rest => Ok(false),
        }
    }

    fn emit_events(
        &self,
        out: &mut String,
        rust_names: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        let mut events = Vec::new();
        for (header, name, kind) in SELECTION {
            if *kind != SelectionKind::Event {
                continue;
            }
            events.push(self.prepare_event(header, name)?);
        }
        let mut codes = HashSet::new();
        for event in &events {
            if event.extension.is_none() && !codes.insert(event.number) {
                return Err(format!("two core events use code {}", event.number));
            }
        }
        for event in &events {
            claim(rust_names, &event.rust_name, format!("{}.{}", event.header, event.xml_name))?;
            self.emit_event_struct(out, event)?;
        }
        claim(rust_names, "ExtensionEvent", "event extension lookup".to_string())?;
        claim(rust_names, "Event", "event enum".to_string())?;
        emit_event_enum(out, &events);
        Ok(())
    }

    fn prepare_event(&self, header: &str, name: &str) -> Result<PreparedEvent, String> {
        let module = self.module(header)?;
        let event = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::Event(event) if event.name == name => Some(event),
                _ => None,
            })
            .ok_or_else(|| format!("{header}.{name} event is not in that module"))?;
        if event.xge {
            return Err(format!("{header}.{name} is an XGE event"));
        }
        let extension = match &module.extension_name {
            Some(extension) => Some(type_name(extension)),
            None => {
                if header != "xproto" {
                    return Err(format!("{header} has no extension name"));
                }
                None
            }
        };
        let rust_name = match &extension {
            Some(prefix) => format!("{prefix}{}", type_name(name)),
            None => type_name(name),
        };
        let message = self.lower_message(
            header,
            &format!("{header}.{name}"),
            &event.fields,
            false,
            false,
            &format!("{rust_name}Aux"),
        )?;
        for field in &message.fields {
            let plain = strip_raw(&field.name);
            if plain == "send_event"
                || plain == "sequence"
                || (extension.is_some() && plain == "code")
            {
                return Err(format!("{header}.{name} field {plain} is reserved"));
            }
        }
        Ok(PreparedEvent {
            header: header.to_string(),
            xml_name: name.to_string(),
            rust_name,
            extension,
            number: event.number,
            has_sequence: !event.no_sequence_number,
            message,
        })
    }

    fn emit_event_struct(&self, out: &mut String, event: &PreparedEvent) -> Result<(), String> {
        let mut fields = Vec::new();
        fields.push(RustField {
            name: "send_event".to_string(),
            ty: "bool".to_string(),
            copy: true,
        });
        if event.extension.is_some() {
            fields.push(RustField { name: "code".to_string(), ty: "u8".to_string(), copy: true });
        }
        if event.has_sequence {
            fields.push(RustField {
                name: "sequence".to_string(),
                ty: "u16".to_string(),
                copy: true,
            });
        }
        fields.extend(event.message.fields.iter().cloned());
        let copy = fields.iter().all(|field| field.copy);
        emit_struct_type(out, &event.rust_name, false, &fields, copy);
        out.push('\n');
        push(out, 0, &format!("impl {} {{", event.rust_name));
        if event.extension.is_some() {
            push(out, 1, "/// Protocol number added to the extension's first event code.");
            push(out, 1, &format!("pub(crate) const NUMBER: u8 = {};", event.number));
        } else {
            push(out, 1, &format!("pub(crate) const CODE: u8 = {};", event.number));
        }
        push(
            out,
            1,
            "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {",
        );
        emit_event_serialize(out, 2, event);
        push(out, 1, "}");
        out.push('\n');
        self.emit_event_parse(out, event)?;
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }

    fn emit_event_parse(&self, out: &mut String, event: &PreparedEvent) -> Result<(), String> {
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, "let raw = reader.get_u8()?;");
        push(out, 2, "let send_event = raw & 0x80 != 0;");
        if event.extension.is_some() {
            push(out, 2, "let code = raw & 0x7f;");
        } else {
            push(out, 2, "debug_assert_eq!(raw & 0x7f, Self::CODE);");
        }
        let mut parsed = Vec::new();
        match &event.message.header {
            HeaderByte::Field(field) => {
                let loaded = read_value(&field.wire, &field.rust_ty);
                push(out, 2, &format!("let {} = {loaded};", field.rust_name));
                parsed.push(field.clone());
            }
            HeaderByte::Zero => push(out, 2, "let _detail = reader.get_u8()?;"),
        }
        if event.has_sequence {
            push(out, 2, "let sequence = reader.get_u16()?;");
        }
        for op in &event.message.ops {
            match op {
                Op::Pad(pad) => emit_pad(out, 2, *pad, false),
                Op::Store(field) | Op::Len(field) => {
                    let loaded = read_value(&field.wire, &field.rust_ty);
                    push(out, 2, &format!("let {} = {loaded};", field.rust_name));
                    parsed.push(field.clone());
                }
                Op::List { field, count, .. } => {
                    self.emit_list_parse(out, &event.header, field, count, &parsed)?;
                }
                Op::Mask { .. } | Op::Aux(_) => {
                    return Err(format!("{} event contains a value list", event.rust_name));
                }
            }
        }
        push(out, 2, "reader.finish_event()?;");
        push(out, 2, "Ok(Self {");
        push(out, 3, "send_event,");
        if event.extension.is_some() {
            push(out, 3, "code,");
        }
        if event.has_sequence {
            push(out, 3, "sequence,");
        }
        for field in &event.message.fields {
            push(out, 3, &format!("{},", field.name));
        }
        push(out, 2, "})");
        push(out, 1, "}");
        Ok(())
    }

    fn emit_errors(
        &self,
        out: &mut String,
        rust_names: &mut HashMap<String, String>,
    ) -> Result<(), String> {
        claim(rust_names, "XError", "x11 error".to_string())?;
        claim(rust_names, "ErrorName", "x11 error name".to_string())?;
        claim(rust_names, "ExtensionError", "error extension lookup".to_string())?;
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, "pub(crate) struct XError {");
        push(out, 1, "pub(crate) code: u8,");
        push(out, 1, "pub(crate) sequence: u16,");
        push(out, 1, "pub(crate) bad_value: u32,");
        push(out, 1, "pub(crate) minor_opcode: u16,");
        push(out, 1, "pub(crate) major_opcode: u8,");
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "impl XError {");
        push(
            out,
            1,
            "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {",
        );
        push(out, 2, "writer.begin_event();");
        push(out, 2, "writer.u8(0);");
        push(out, 2, "writer.u8(self.code);");
        push(out, 2, "writer.u16(self.sequence);");
        push(out, 2, "writer.u32(self.bad_value);");
        push(out, 2, "writer.u16(self.minor_opcode);");
        push(out, 2, "writer.u8(self.major_opcode);");
        push(out, 2, "writer.finish_event()");
        push(out, 1, "}");
        out.push('\n');
        push(out, 1, "pub(crate) fn parse(reader: &mut Reader<'_>) -> Result<Self, WireError> {");
        push(out, 2, "let _response = reader.get_u8()?;");
        push(out, 2, "let code = reader.get_u8()?;");
        push(out, 2, "let sequence = reader.get_u16()?;");
        push(out, 2, "let bad_value = reader.get_u32()?;");
        push(out, 2, "let minor_opcode = reader.get_u16()?;");
        push(out, 2, "let major_opcode = reader.get_u8()?;");
        push(out, 2, "reader.finish_event()?;");
        push(out, 2, "Ok(Self { code, sequence, bad_value, minor_opcode, major_opcode })");
        push(out, 1, "}");
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, "pub(crate) struct ExtensionError {");
        push(out, 1, "pub(crate) extension: ExtensionName,");
        push(out, 1, "pub(crate) first_error: u8,");
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
        push(out, 0, "pub(crate) struct ErrorName {");
        push(out, 1, "pub(crate) extension: Option<ExtensionName>,");
        push(out, 1, "pub(crate) number: u8,");
        push(out, 1, "pub(crate) name: &'static str,");
        push(out, 0, "}");
        out.push('\n');
        push(out, 0, "pub(crate) const ERROR_NAMES: &[ErrorName] = &[");
        for module in &self.modules {
            let extension = module.extension_name.as_deref().map(type_name);
            for item in &module.items {
                let Item::Error(error) = item else { continue };
                let extension = match &extension {
                    Some(name) => format!("Some(ExtensionName::{name})"),
                    None => "None".to_string(),
                };
                push(
                    out,
                    1,
                    &format!(
                        "ErrorName {{ extension: {extension}, number: {}, name: \"{}\" }},",
                        error.number, error.name
                    ),
                );
            }
        }
        push(out, 0, "];");
        out.push('\n');
        push(
            out,
            0,
            "pub(crate) fn error_name(code: u8, extensions: &[ExtensionError]) -> Option<&'static str> {",
        );
        push(out, 1, "if code < 128 {");
        push(out, 2, "return ERROR_NAMES.iter().find(|error| {");
        push(out, 3, "error.extension.is_none() && error.number == code");
        push(out, 2, "}).map(|error| error.name);");
        push(out, 1, "}");
        push(out, 1, "for extension in extensions {");
        push(out, 2, "let Some(minor) = code.checked_sub(extension.first_error) else {");
        push(out, 3, "continue;");
        push(out, 2, "};");
        push(out, 2, "if let Some(error) = ERROR_NAMES.iter().find(|error| {");
        push(out, 3, "error.extension == Some(extension.extension) && error.number == minor");
        push(out, 2, "}) {");
        push(out, 3, "return Some(error.name);");
        push(out, 2, "}");
        push(out, 1, "}");
        push(out, 1, "None");
        push(out, 0, "}");
        out.push('\n');
        Ok(())
    }
}

struct PreparedEvent {
    header: String,
    xml_name: String,
    rust_name: String,
    extension: Option<String>,
    number: u8,
    has_sequence: bool,
    message: Message,
}

struct Message {
    header: HeaderByte,
    ops: Vec<Op>,
    fields: Vec<RustField>,
    lengths: Vec<LengthLocal>,
    checks: Vec<String>,
    aux: Option<AuxDef>,
    lifetime: bool,
}

enum HeaderByte {
    Zero,
    Field(Encoded),
}

#[derive(Clone)]
struct RustField {
    name: String,
    ty: String,
    copy: bool,
}

struct LengthLocal {
    local: String,
    ty: String,
    list: String,
}

enum Op {
    Pad(Pad),
    Store(Encoded),
    Len(Encoded),
    Mask { aux: String, width: Prim },
    List { field: Encoded, count: ListCount, borrow: bool },
    Aux(String),
}

enum ListCount {
    Fixed(usize),
    Expr(Expr),
    Rest,
}

struct AuxDef {
    mask_ty: String,
    cases: Vec<AuxCase>,
    copy: bool,
}

struct SwitchInfo {
    field_name: String,
    mask_xml: String,
    mask: Encoded,
    width: Prim,
    cases: Vec<AuxCase>,
    copy: bool,
}

struct AuxCase {
    bit: u8,
    flag: String,
    field: Encoded,
    ops: Vec<CaseOp>,
}

enum CaseOp {
    Pad(Pad),
    Field,
}

enum SerMode {
    Core,
    Extension,
    Reply,
}

fn claim(
    rust_names: &mut HashMap<String, String>,
    rust_name: &str,
    source: String,
) -> Result<(), String> {
    if let Some(previous) = rust_names.insert(rust_name.to_string(), source.clone()) {
        return Err(format!(
            "rust type {rust_name} would be generated for both {previous} and {source}"
        ));
    }
    Ok(())
}

fn derives(copy: bool, default: bool) -> &'static str {
    match (copy, default) {
        (true, true) => "#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]",
        (true, false) => "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]",
        (false, true) => "#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]",
        (false, false) => "#[derive(Clone, Debug, PartialEq, Eq, Hash)]",
    }
}

fn message_is_copy(message: &Message) -> bool {
    message.fields.iter().all(|field| field.copy)
}

fn emit_struct_type(
    out: &mut String,
    name: &str,
    lifetime: bool,
    fields: &[RustField],
    copy: bool,
) {
    push(out, 0, derives(copy, false));
    if fields.is_empty() {
        push(out, 0, &format!("pub(crate) struct {name};"));
        return;
    }
    let lifetime = if lifetime { "<'a>" } else { "" };
    push(out, 0, &format!("pub(crate) struct {name}{lifetime} {{"));
    for field in fields {
        push(out, 1, &format!("pub(crate) {}: {},", field.name, field.ty));
    }
    push(out, 0, "}");
}

fn emit_event_enum(out: &mut String, events: &[PreparedEvent]) {
    push(out, 0, "#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]");
    push(out, 0, "pub(crate) struct ExtensionEvent {");
    push(out, 1, "pub(crate) extension: ExtensionName,");
    push(out, 1, "pub(crate) first_event: u8,");
    push(out, 0, "}");
    out.push('\n');
    push(out, 0, "#[derive(Clone, Debug, PartialEq, Eq, Hash)]");
    push(out, 0, "pub(crate) enum Event {");
    for event in events {
        push(out, 1, &format!("{}({}),", event.rust_name, event.rust_name));
    }
    push(out, 1, "Unknown { send_event: bool, code: u8, bytes: Vec<u8> },");
    push(out, 0, "}");
    out.push('\n');
    push(out, 0, "impl Event {");
    push(out, 1, "pub(crate) fn send_event(&self) -> bool {");
    push(out, 2, "match self {");
    for event in events {
        push(out, 3, &format!("Self::{}(event) => event.send_event,", event.rust_name));
    }
    push(out, 3, "Self::Unknown { send_event, .. } => *send_event,");
    push(out, 2, "}");
    push(out, 1, "}");
    out.push('\n');
    push(
        out,
        1,
        "pub(crate) fn parse(data: &[u8], extensions: &[ExtensionEvent]) -> Result<Self, WireError> {",
    );
    push(out, 2, "if data.is_empty() {");
    push(out, 3, "return Err(WireError::Short);");
    push(out, 2, "}");
    push(out, 2, "let code = data[0] & 0x7f;");
    push(out, 2, "let mut reader = Reader::new(data);");
    push(out, 2, "let event = match code {");
    for event in events {
        if event.extension.is_some() {
            continue;
        }
        push(
            out,
            3,
            &format!(
                "{}::CODE => Self::{}({}::parse(&mut reader)?),",
                event.rust_name, event.rust_name, event.rust_name
            ),
        );
    }
    push(out, 3, "35 => return Self::unknown(data),");
    push(out, 3, "code => {");
    push(out, 4, "for extension in extensions {");
    push(out, 5, "let Some(minor) = code.checked_sub(extension.first_event) else {");
    push(out, 6, "continue;");
    push(out, 5, "};");
    push(out, 5, "match (extension.extension, minor) {");
    for event in events {
        let Some(extension) = &event.extension else { continue };
        push(out, 6, &format!("(ExtensionName::{extension}, {}::NUMBER) => {{", event.rust_name));
        push(
            out,
            7,
            &format!(
                "return Ok(Self::{}({}::parse(&mut reader)?));",
                event.rust_name, event.rust_name
            ),
        );
        push(out, 6, "}");
        push(out, 6, ",");
    }
    push(out, 6, "_ => continue,");
    push(out, 5, "}");
    push(out, 4, "}");
    push(out, 4, "return Self::unknown(data);");
    push(out, 3, "}");
    push(out, 2, "};");
    push(out, 2, "if reader.remaining() != 0 {");
    push(out, 3, "return Err(WireError::RequestTooLong);");
    push(out, 2, "}");
    push(out, 2, "Ok(event)");
    push(out, 1, "}");
    out.push('\n');
    push(
        out,
        1,
        "pub(crate) fn serialize(&self, writer: &mut Writer<'_>) -> Result<(), WireError> {",
    );
    push(out, 2, "match self {");
    for event in events {
        push(out, 3, &format!("Self::{}(event) => event.serialize(writer),", event.rust_name));
    }
    push(out, 3, "Self::Unknown { send_event, code, bytes } => {");
    push(out, 4, "if bytes.is_empty() {");
    push(out, 5, "return Err(WireError::Short);");
    push(out, 4, "}");
    push(out, 4, "let mut bytes = bytes.clone();");
    push(out, 4, "bytes[0] = code & 0x7f;");
    push(out, 4, "if *send_event {");
    push(out, 5, "bytes[0] |= 0x80;");
    push(out, 4, "}");
    push(out, 4, "writer.bytes(&bytes);");
    push(out, 4, "Ok(())");
    push(out, 3, "}");
    push(out, 2, "}");
    push(out, 1, "}");
    out.push('\n');
    push(out, 1, "fn unknown(data: &[u8]) -> Result<Self, WireError> {");
    push(out, 2, "if data.is_empty() {");
    push(out, 3, "return Err(WireError::Short);");
    push(out, 2, "}");
    push(out, 2, "let mut bytes = data.to_vec();");
    push(out, 2, "let send_event = bytes[0] & 0x80 != 0;");
    push(out, 2, "bytes[0] &= 0x7f;");
    push(out, 2, "let code = bytes[0];");
    push(out, 2, "Ok(Self::Unknown { send_event, code, bytes })");
    push(out, 1, "}");
    push(out, 0, "}");
    out.push('\n');
}

fn emit_event_serialize(out: &mut String, indent: usize, event: &PreparedEvent) {
    for check in &event.message.checks {
        push(out, indent, check);
    }
    for length in &event.message.lengths {
        push(
            out,
            indent,
            &format!(
                "let {} = {}::try_from(self.{}.len()).map_err(|_| WireError::RequestTooLong)?;",
                length.local, length.ty, length.list
            ),
        );
    }
    push(out, indent, "writer.begin_event();");
    if event.extension.is_some() {
        push(
            out,
            indent,
            "writer.u8(if self.send_event { self.code | 0x80 } else { self.code & 0x7f });",
        );
    } else {
        push(
            out,
            indent,
            "writer.u8(if self.send_event { Self::CODE | 0x80 } else { Self::CODE });",
        );
    }
    let data = data_byte_expr(&event.message.header);
    push(out, indent, &format!("writer.u8({data});"));
    if event.has_sequence {
        push(out, indent, "writer.u16(self.sequence);");
    }
    for op in &event.message.ops {
        match op {
            Op::Pad(pad) => emit_pad(out, indent, *pad, true),
            Op::Store(field) => {
                let value = format!("self.{}", field.rust_name);
                push(out, indent, &write_value(&field.wire, &value));
            }
            Op::Len(field) => {
                let Wire::Prim(prim) = field.wire else {
                    unreachable!("length fields are unsigned primitives");
                };
                push(out, indent, &prim.write_call(&field.rust_name));
            }
            Op::Mask { aux, width } => push(out, indent, &mask_write(aux, *width)),
            Op::List { field, borrow, .. } => emit_list_write(out, indent, field, *borrow),
            Op::Aux(name) => push(out, indent, &format!("self.{name}.write(writer);")),
        }
    }
    push(out, indent, "writer.finish_event()");
}

fn emit_serialize_stmts(out: &mut String, indent: usize, message: &Message, mode: SerMode) {
    if !matches!(mode, SerMode::Reply) && !serialize_uses_self(message) {
        push(out, indent, "let _ = self;");
    }
    for check in &message.checks {
        push(out, indent, check);
    }
    for length in &message.lengths {
        push(
            out,
            indent,
            &format!(
                "let {} = {}::try_from(self.{}.len()).map_err(|_| WireError::RequestTooLong)?;",
                length.local, length.ty, length.list
            ),
        );
    }
    let data = data_byte_expr(&message.header);
    match mode {
        SerMode::Core => push(out, indent, &format!("writer.begin_request(Self::OPCODE, {data});")),
        SerMode::Extension => {
            push(out, indent, "writer.begin_request(major_opcode, Self::OPCODE);")
        }
        SerMode::Reply => push(out, indent, &format!("writer.begin_reply({data}, self.sequence);")),
    }
    for op in &message.ops {
        match op {
            Op::Pad(pad) => emit_pad(out, indent, *pad, true),
            Op::Store(field) => {
                let value = format!("self.{}", field.rust_name);
                push(out, indent, &write_value(&field.wire, &value));
            }
            Op::Len(field) => {
                let Wire::Prim(prim) = field.wire else {
                    unreachable!("length fields are unsigned primitives");
                };
                push(out, indent, &prim.write_call(&field.rust_name));
            }
            Op::Mask { aux, width } => push(out, indent, &mask_write(aux, *width)),
            Op::List { field, borrow, .. } => emit_list_write(out, indent, field, *borrow),
            Op::Aux(name) => push(out, indent, &format!("self.{name}.write(writer);")),
        }
    }
    match mode {
        SerMode::Reply => push(out, indent, "writer.finish_reply()"),
        SerMode::Core | SerMode::Extension => push(out, indent, "writer.finish_request()"),
    }
}

fn serialize_uses_self(message: &Message) -> bool {
    if matches!(message.header, HeaderByte::Field(_)) {
        return true;
    }
    if !message.lengths.is_empty() || !message.checks.is_empty() {
        return true;
    }
    message.ops.iter().any(|op| {
        matches!(op, Op::Store(_) | Op::Len(_) | Op::Mask { .. } | Op::List { .. } | Op::Aux(_))
    })
}

fn data_byte_expr(header: &HeaderByte) -> String {
    let HeaderByte::Field(field) = header else {
        return "0".to_string();
    };
    let value = format!("self.{}", field.rust_name);
    match &field.wire {
        Wire::Prim(Prim::U8) => value,
        Wire::Prim(Prim::I8) => format!("{value} as u8"),
        Wire::ValueEnum { width: Prim::U8 | Prim::I8 } => format!("{value}.0 as u8"),
        Wire::Bits { width: Prim::U8 } => format!("{value}.bits() as u8"),
        _ => unreachable!("header data is a one-byte field"),
    }
}

fn mask_write(aux: &str, width: Prim) -> String {
    let bits = format!("self.{aux}.mask().bits()");
    match width {
        Prim::U8 => format!("writer.u8({bits} as u8);"),
        Prim::U16 => format!("writer.u16({bits} as u16);"),
        Prim::U32 => format!("writer.u32({bits});"),
        Prim::I8 | Prim::I16 | Prim::I32 => unreachable!("value masks are unsigned"),
    }
}

fn emit_list_write(out: &mut String, indent: usize, field: &Encoded, borrow: bool) {
    if is_byte_blob(&field.wire) {
        if borrow {
            push(out, indent, &format!("writer.bytes(self.{});", field.rust_name));
        } else {
            push(out, indent, &format!("writer.bytes(&self.{});", field.rust_name));
        }
        return;
    }
    if borrow {
        push(out, indent, &format!("for item in self.{} {{", field.rust_name));
    } else {
        push(out, indent, &format!("for item in &self.{} {{", field.rust_name));
    }
    let value = list_item_expr(&field.wire);
    push(out, indent + 1, &write_value(&field.wire, &value));
    push(out, indent, "}");
}

fn emit_pad(out: &mut String, indent: usize, pad: Pad, write: bool) {
    match (write, pad) {
        (true, Pad::Bytes(count)) => push(out, indent, &format!("writer.pad({count});")),
        (true, Pad::Align(count)) => push(out, indent, &format!("writer.align({count});")),
        (false, Pad::Bytes(count)) => push(out, indent, &format!("reader.skip({count})?;")),
        (false, Pad::Align(count)) => push(out, indent, &format!("reader.align({count})?;")),
    }
}

fn emit_aux_field_write(out: &mut String, indent: usize, field: &Encoded) {
    let line = match &field.wire {
        Wire::Prim(prim) => prim.write_call("*value"),
        wire => write_value(wire, "value"),
    };
    push(out, indent, &line);
}

fn push_rust_field(
    fields: &mut Vec<RustField>,
    seen: &mut HashSet<String>,
    owner: &str,
    field: &Encoded,
    ty: String,
    copy: bool,
) -> Result<(), String> {
    if !seen.insert(field.rust_name.clone()) {
        return Err(format!("{owner} field {} is duplicated", field.rust_name));
    }
    fields.push(RustField { name: field.rust_name.clone(), ty, copy });
    Ok(())
}

fn is_one_byte(wire: &Wire) -> bool {
    matches!(
        wire,
        Wire::Prim(Prim::U8 | Prim::I8)
            | Wire::ValueEnum { width: Prim::U8 | Prim::I8 }
            | Wire::Bits { width: Prim::U8 }
    )
}

fn unsigned_length(wire: &Wire) -> Option<Prim> {
    match wire {
        Wire::Prim(prim @ (Prim::U8 | Prim::U16 | Prim::U32)) => Some(*prim),
        _ => None,
    }
}

fn list_rust_type(inner: &str, count: &ListCount, borrow: bool) -> String {
    match (borrow, count) {
        (true, ListCount::Fixed(count)) => format!("&'a [{inner}; {count}]"),
        (true, _) => format!("&'a [{inner}]"),
        (false, ListCount::Fixed(count)) => format!("[{inner}; {count}]"),
        (false, _) => format!("Vec<{inner}>"),
    }
}

/// Which declaration a name refers to.
///
/// An xcb file can reuse a name for an error and an enum (`Cursor`, `Pixmap`,
/// `Font`). A type or enum reference means the type, not the error.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Want {
    Type,
    Request,
    Event,
}

impl Want {
    fn matches(self, item: &Item) -> bool {
        match self {
            Self::Type => matches!(
                item,
                Item::XidType(_)
                    | Item::XidUnion { .. }
                    | Item::Typedef { .. }
                    | Item::Enum { .. }
                    | Item::Struct { .. }
                    | Item::Union { .. }
            ),
            Self::Request => matches!(item, Item::Request(_)),
            Self::Event => matches!(item, Item::Event(_)),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Request => "request",
            Self::Event => "event",
        }
    }
}

fn expr_usize(
    expr: &Expr,
    from: &str,
    fields: &[Encoded],
    prefix: &str,
    gen: &Gen,
) -> Result<String, String> {
    match expr {
        Expr::Value(value) => Ok(format!("{value}usize")),
        Expr::FieldRef(name) => {
            let field = fields.iter().find(|field| field.xml_name == *name).ok_or_else(|| {
                format!("length expression references {name}, which is not a previous numeric field")
            })?;
            numeric_usize(&field.wire, &format!("{prefix}{}", field.rust_name))
                .ok_or_else(|| format!("length expression references non-numeric field {name}"))
        }
        Expr::EnumRef { enum_name, item } => {
            let (header, found) = gen.resolve_owned(from, enum_name, Want::Type)?;
            let Item::Enum { items, .. } = found else {
                return Err(format!("{header}.{enum_name} is not an enum"));
            };
            let bits = items
                .iter()
                .any(|enum_item| matches!(enum_item.value, EnumValue::Bit(_)));
            let const_name = const_ident(item)?;
            let rust = type_name(enum_name);
            if bits {
                Ok(format!("({rust}::{const_name}.bits() as usize)"))
            } else {
                Ok(format!("({rust}::{const_name}.0 as usize)"))
            }
        }
        Expr::Op { op, left, right } => {
            let left = expr_usize(left, from, fields, prefix, gen)?;
            let right = expr_usize(right, from, fields, prefix, gen)?;
            Ok(format!("({left} {} {right})", op_symbol(*op)))
        }
    }
}

fn numeric_usize(wire: &Wire, expr: &str) -> Option<String> {
    match wire {
        Wire::Prim(_) => Some(format!("({expr} as usize)")),
        Wire::ValueEnum { .. } => Some(format!("({expr}.0 as usize)")),
        Wire::Bits { .. } => Some(format!("({expr}.bits() as usize)")),
        Wire::Xid | Wire::Struct | Wire::Union => None,
    }
}

fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::BitAnd => "&",
        BinOp::Shl => "<<",
    }
}

fn write_value(wire: &Wire, value: &str) -> String {
    match wire {
        Wire::Prim(prim) => prim.write_call(value),
        Wire::Xid | Wire::Struct | Wire::Union => format!("{value}.serialize(writer);"),
        Wire::ValueEnum { width } => match width {
            Prim::U8 | Prim::I8 => format!("writer.u8({value}.0 as u8);"),
            Prim::U16 => format!("writer.u16({value}.0 as u16);"),
            Prim::I16 => format!("writer.i16({value}.0 as i16);"),
            Prim::U32 => format!("writer.u32({value}.0);"),
            Prim::I32 => format!("writer.i32({value}.0 as i32);"),
        },
        Wire::Bits { width } => match width {
            Prim::U8 => format!("writer.u8({value}.bits() as u8);"),
            Prim::U16 => format!("writer.u16({value}.bits() as u16);"),
            Prim::U32 => format!("writer.u32({value}.bits());"),
            Prim::I8 | Prim::I16 | Prim::I32 => {
                unreachable!("bitflags are emitted only for unsigned widths")
            }
        },
    }
}

fn read_value(wire: &Wire, rust: &str) -> String {
    match wire {
        Wire::Prim(prim) => prim.read_call(),
        Wire::Xid | Wire::Struct | Wire::Union => format!("{rust}::parse(reader)?"),
        Wire::ValueEnum { width } => match width {
            Prim::U8 | Prim::I8 => format!("{rust}(u32::from(reader.get_u8()?))"),
            Prim::U16 => format!("{rust}(u32::from(reader.get_u16()?))"),
            Prim::I16 => format!("{rust}(reader.get_i16()? as u32)"),
            Prim::U32 => format!("{rust}(reader.get_u32()?)"),
            Prim::I32 => format!("{rust}(reader.get_i32()? as u32)"),
        },
        Wire::Bits { width } => match width {
            Prim::U8 => format!("{rust}::from_bits_retain(u32::from(reader.get_u8()?))"),
            Prim::U16 => format!("{rust}::from_bits_retain(u32::from(reader.get_u16()?))"),
            Prim::U32 => format!("{rust}::from_bits_retain(reader.get_u32()?)"),
            Prim::I8 | Prim::I16 | Prim::I32 => {
                unreachable!("bitflags are emitted only for unsigned widths")
            }
        },
    }
}

fn list_item_expr(wire: &Wire) -> String {
    match wire {
        Wire::Prim(_) => "*item".into(),
        Wire::Xid | Wire::Struct | Wire::Union | Wire::ValueEnum { .. } | Wire::Bits { .. } => {
            "item".into()
        }
    }
}

fn is_byte_blob(wire: &Wire) -> bool {
    matches!(wire, Wire::Prim(Prim::U8))
}

fn scalar_fields(parts: &[Part]) -> Vec<Encoded> {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Field(field) => Some(field.clone()),
            _ => None,
        })
        .collect()
}

fn item_name(item: &Item) -> &str {
    match item {
        Item::XidType(name)
        | Item::XidUnion { name, .. }
        | Item::Enum { name, .. }
        | Item::Struct { name, .. }
        | Item::Union { name, .. } => name,
        Item::Typedef { new_name, .. } => new_name,
        Item::Request(request) => &request.name,
        Item::Event(event) => &event.name,
        Item::Error(error) => &error.name,
    }
}

fn type_name(xml_name: &str) -> String {
    xml_name.to_upper_camel_case()
}

fn primitive(name: &str) -> Option<Prim> {
    match name {
        "CARD8" | "BYTE" | "BOOL" | "char" | "void" => Some(Prim::U8),
        "CARD16" => Some(Prim::U16),
        "CARD32" => Some(Prim::U32),
        "INT8" => Some(Prim::I8),
        "INT16" => Some(Prim::I16),
        "INT32" => Some(Prim::I32),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum Prim {
    U8,
    U16,
    U32,
    I8,
    I16,
    I32,
}

impl Prim {
    fn rust(self) -> &'static str {
        match self {
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
        }
    }

    fn size(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 => 4,
        }
    }

    fn write_call(self, value: &str) -> String {
        match self {
            Self::U8 => format!("writer.u8({value});"),
            Self::U16 => format!("writer.u16({value});"),
            Self::U32 => format!("writer.u32({value});"),
            Self::I8 => format!("writer.u8({value} as u8);"),
            Self::I16 => format!("writer.i16({value});"),
            Self::I32 => format!("writer.i32({value});"),
        }
    }

    fn read_call(self) -> String {
        match self {
            Self::U8 => "reader.get_u8()?".into(),
            Self::U16 => "reader.get_u16()?".into(),
            Self::U32 => "reader.get_u32()?".into(),
            Self::I8 => "i8::from_ne_bytes([reader.get_u8()?])".into(),
            Self::I16 => "reader.get_i16()?".into(),
            Self::I32 => "reader.get_i32()?".into(),
        }
    }

    fn read_expr(self, bytes: &str, start: usize) -> String {
        match self {
            Self::U8 => format!("{bytes}[{start}]"),
            Self::I8 => format!("i8::from_ne_bytes([{bytes}[{start}]])"),
            Self::U16 => {
                format!("u16::from_ne_bytes({bytes}[{start}..{start} + 2].try_into().unwrap())")
            }
            Self::I16 => {
                format!("i16::from_ne_bytes({bytes}[{start}..{start} + 2].try_into().unwrap())")
            }
            Self::U32 => {
                format!("u32::from_ne_bytes({bytes}[{start}..{start} + 4].try_into().unwrap())")
            }
            Self::I32 => {
                format!("i32::from_ne_bytes({bytes}[{start}..{start} + 4].try_into().unwrap())")
            }
        }
    }

    fn read_slot(self) -> String {
        match self {
            Self::U8 => "*slot = self.bytes[start];".into(),
            Self::I8 => "*slot = i8::from_ne_bytes([self.bytes[start]]);".into(),
            Self::U16 => {
                "*slot = u16::from_ne_bytes(self.bytes[start..start + 2].try_into().unwrap());"
                    .into()
            }
            Self::I16 => {
                "*slot = i16::from_ne_bytes(self.bytes[start..start + 2].try_into().unwrap());"
                    .into()
            }
            Self::U32 => {
                "*slot = u32::from_ne_bytes(self.bytes[start..start + 4].try_into().unwrap());"
                    .into()
            }
            Self::I32 => {
                "*slot = i32::from_ne_bytes(self.bytes[start..start + 4].try_into().unwrap());"
                    .into()
            }
        }
    }

    fn write_at(self, _bytes: &str, value: &str, start: usize) -> String {
        let width = self.size();
        format!("self.bytes[{start}..{start} + {width}].copy_from_slice(&{value}.to_ne_bytes());")
    }
}

enum Class {
    Prim(Prim),
    Alias { rust: String, inner: Box<Class> },
    Xid { rust: String },
    Struct { rust: String, header: String, xml: String },
    Union { rust: String },
    Value { rust: String },
    Bits { rust: String },
}

impl Class {
    fn rust_name(&self) -> String {
        match self {
            Self::Prim(prim) => prim.rust().into(),
            Self::Alias { rust, .. }
            | Self::Xid { rust }
            | Self::Struct { rust, .. }
            | Self::Union { rust }
            | Self::Value { rust }
            | Self::Bits { rust } => rust.clone(),
        }
    }

    fn prim_width(&self) -> Option<Prim> {
        match self {
            Self::Prim(prim) => Some(*prim),
            Self::Alias { inner, .. } => inner.prim_width(),
            _ => None,
        }
    }

    fn to_wire(&self) -> Result<WireInfo, String> {
        match self {
            Self::Prim(prim) => Ok(WireInfo {
                rust: prim.rust().into(),
                wire: Wire::Prim(*prim),
                nested: None,
            }),
            Self::Alias { rust, inner } => {
                let mut info = inner.to_wire()?;
                info.rust = rust.clone();
                Ok(info)
            }
            Self::Xid { rust } => {
                Ok(WireInfo { rust: rust.clone(), wire: Wire::Xid, nested: None })
            }
            Self::Struct { rust, header, xml } => Ok(WireInfo {
                rust: rust.clone(),
                wire: Wire::Struct,
                nested: Some((header.clone(), xml.clone())),
            }),
            Self::Union { rust } => {
                Ok(WireInfo { rust: rust.clone(), wire: Wire::Union, nested: None })
            }
            Self::Value { rust } => Err(format!("enum {rust} is used as a field type")),
            Self::Bits { rust } => Err(format!("enum {rust} is used as a field type")),
        }
    }
}

struct WireInfo {
    rust: String,
    wire: Wire,
    nested: Option<(String, String)>,
}

#[derive(Clone)]
enum Wire {
    Prim(Prim),
    Xid,
    Struct,
    Union,
    ValueEnum { width: Prim },
    Bits { width: Prim },
}

impl Wire {
    fn as_prim(&self) -> Option<Prim> {
        match self {
            Self::Prim(prim) => Some(*prim),
            _ => None,
        }
    }
}

#[derive(Clone)]
struct Encoded {
    xml_name: String,
    rust_name: String,
    rust_ty: String,
    wire: Wire,
    nested: Option<(String, String)>,
}

enum Part {
    Pad(Pad),
    Field(Encoded),
    List { field: Encoded, len: ListLen },
}

enum ListLen {
    Constant(usize),
    Dynamic(Expr),
}

struct UnionMember {
    name: String,
    prim: Prim,
    count: usize,
    bytes: usize,
}

fn push(out: &mut String, indent: usize, line: &str) {
    for _ in 0..indent {
        out.push_str("    ");
    }
    out.push_str(line);
    out.push('\n');
}

fn ident(name: &str) -> Result<String, String> {
    let mut ident = name.to_string();
    if ident.starts_with(|c: char| c.is_ascii_digit()) {
        ident.insert_str(0, "N");
    }
    let valid = ident.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic())
        && ident.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
    if !valid {
        return Err(format!("invalid rust identifier {name}"));
    }
    if is_keyword(&ident) {
        Ok(format!("r#{ident}"))
    } else {
        Ok(ident)
    }
}

fn const_ident(name: &str) -> Result<String, String> {
    ident(name)
}

fn strip_raw(ident: &str) -> &str {
    ident.strip_prefix("r#").unwrap_or(ident)
}

fn is_keyword(name: &str) -> bool {
    matches!(
        name,
        "as" | "async"
            | "await"
            | "break"
            | "const"
            | "continue"
            | "crate"
            | "dyn"
            | "else"
            | "enum"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "mod"
            | "move"
            | "mut"
            | "pub"
            | "ref"
            | "return"
            | "self"
            | "Self"
            | "static"
            | "struct"
            | "super"
            | "trait"
            | "true"
            | "type"
            | "unsafe"
            | "use"
            | "where"
            | "while"
            | "union"
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, process::Command};

    use {super::generate, crate::x11::generated::Request};

    fn generated_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/x11/generated.rs")
    }

    fn format_generated(source: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "tema-x11-{}-{}.rs",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        fs::write(&path, source).unwrap();
        let output = Command::new("rustfmt")
            .arg("--edition=2021")
            .arg(format!("--config-path={}/rustfmt.toml", env!("CARGO_MANIFEST_DIR")))
            .arg(&path)
            .output()
            .expect("run rustfmt");
        assert!(
            output.status.success(),
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let formatted = fs::read_to_string(&path).unwrap();
        let _ = fs::remove_file(&path);
        formatted
    }

    #[test]
    #[ignore = "rewrites src/x11/generated.rs; run: cargo test regenerate_x11_generated -- --ignored"]
    fn regenerate_x11_generated() {
        let formatted = format_generated(&generate().unwrap());
        fs::write(generated_path(), formatted).unwrap();
    }

    #[test]
    fn generated_source_matches_installed_xcb() {
        if !PathBuf::from("/usr/share/xcb/xproto.xml").exists() {
            return;
        }
        let fresh = format_generated(&generate().unwrap());
        let checked_in = fs::read_to_string(generated_path()).unwrap();
        assert_eq!(fresh, checked_in);
    }

    #[test]
    fn setup_and_screen_round_trip() {
        use crate::x11::{
            generated::{
                Atom, BackingStore, Colormap, Depth, EventMask, Format, ImageOrder, Screen, Setup,
                VisualClass, Visualtype, Window,
            },
            wire::{Reader, Writer},
        };

        assert_eq!(Atom::PRIMARY.0, 1);
        assert_eq!(Window::None.0, 0);

        let screen = Screen {
            root: Window(1),
            default_colormap: Colormap(2),
            white_pixel: 0x00ff_ffff,
            black_pixel: 0,
            current_input_masks: EventMask::KeyPress | EventMask::StructureNotify,
            width_in_pixels: 800,
            height_in_pixels: 600,
            width_in_millimeters: 200,
            height_in_millimeters: 150,
            min_installed_maps: 1,
            max_installed_maps: 1,
            root_visual: 0x21,
            backing_stores: BackingStore::WhenMapped,
            save_unders: 0,
            root_depth: 24,
            allowed_depths_len: 1,
            allowed_depths: vec![Depth {
                depth: 24,
                visuals_len: 1,
                visuals: vec![Visualtype {
                    visual_id: 0x21,
                    class: VisualClass::TrueColor,
                    bits_per_rgb_value: 8,
                    colormap_entries: 256,
                    red_mask: 0x00ff_0000,
                    green_mask: 0x0000_ff00,
                    blue_mask: 0x0000_00ff,
                }],
            }],
        };
        let mut screen_bytes = Vec::new();
        screen.serialize(&mut Writer::new(&mut screen_bytes));
        let mut reader = Reader::new(&screen_bytes);
        assert_eq!(Screen::parse(&mut reader).unwrap(), screen);
        assert_eq!(reader.remaining(), 0);

        let setup = Setup {
            status: 1,
            protocol_major_version: 11,
            protocol_minor_version: 0,
            length: 0,
            release_number: 1,
            resource_id_base: 0x0020_0000,
            resource_id_mask: 0x001f_ffff,
            motion_buffer_size: 256,
            vendor_len: 3,
            maximum_request_length: 65535,
            roots_len: 1,
            pixmap_formats_len: 1,
            image_byte_order: ImageOrder::LSBFirst,
            bitmap_format_bit_order: ImageOrder::MSBFirst,
            bitmap_format_scanline_unit: 32,
            bitmap_format_scanline_pad: 32,
            min_keycode: 8,
            max_keycode: 255,
            vendor: b"tem".to_vec(),
            pixmap_formats: vec![Format { depth: 24, bits_per_pixel: 32, scanline_pad: 32 }],
            roots: vec![screen],
        };
        let mut setup_bytes = Vec::new();
        setup.serialize(&mut Writer::new(&mut setup_bytes));
        let mut reader = Reader::new(&setup_bytes);
        assert_eq!(Setup::parse(&mut reader).unwrap(), setup);
        assert_eq!(reader.remaining(), 0);
    }

    fn request_bytes(request: &impl crate::x11::generated::Request, major: u8) -> Vec<u8> {
        use crate::x11::wire::Writer;
        let mut buf = Vec::new();
        request
            .serialize(&mut Writer::new(&mut buf), major)
            .unwrap();
        buf
    }

    #[test]
    fn intern_atom_request_bytes() {
        use crate::x11::generated::InternAtom;

        let name = b"WM_NAME";
        let bytes = request_bytes(&InternAtom { only_if_exists: 0, name }, 0);
        let mut expected = vec![InternAtom::OPCODE, 0];
        expected.extend(4u16.to_ne_bytes());
        expected.extend((name.len() as u16).to_ne_bytes());
        expected.extend_from_slice(&[0, 0]);
        expected.extend_from_slice(name);
        expected.push(0);
        assert_eq!(InternAtom::OPCODE, 16);
        assert_eq!(InternAtom::EXTENSION, None);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn change_property_request_bytes() {
        use crate::x11::generated::{Atom, ChangeProperty, PropMode, Window};

        let data = b"abc";
        let bytes = request_bytes(
            &ChangeProperty {
                mode: PropMode::Replace,
                window: Window(1),
                property: Atom::WM_NAME,
                r#type: Atom::STRING,
                format: 8,
                data_len: data.len() as u32,
                data,
            },
            0,
        );
        let mut expected = vec![ChangeProperty::OPCODE, PropMode::Replace.0 as u8];
        expected.extend(7u16.to_ne_bytes());
        expected.extend(1u32.to_ne_bytes());
        expected.extend(Atom::WM_NAME.0.to_ne_bytes());
        expected.extend(Atom::STRING.0.to_ne_bytes());
        expected.push(8);
        expected.extend_from_slice(&[0, 0, 0]);
        expected.extend((data.len() as u32).to_ne_bytes());
        expected.extend_from_slice(data);
        expected.push(0);
        assert_eq!(ChangeProperty::OPCODE, 18);
        assert_eq!(PropMode::Replace.0, 0);
        assert_eq!(Atom::WM_NAME.0, 39);
        assert_eq!(Atom::STRING.0, 31);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn configure_window_request_bytes() {
        use crate::x11::generated::{ConfigWindow, ConfigureWindow, ConfigureWindowAux, Window};

        let mask = ConfigWindow::X | ConfigWindow::Width;
        let bytes = request_bytes(
            &ConfigureWindow {
                window: Window(7),
                value_list: ConfigureWindowAux::default().x(5).width(200),
            },
            0,
        );
        let mut expected = vec![ConfigureWindow::OPCODE, 0];
        expected.extend(5u16.to_ne_bytes());
        expected.extend(7u32.to_ne_bytes());
        expected.extend((mask.bits() as u16).to_ne_bytes());
        expected.extend_from_slice(&[0, 0]);
        expected.extend(5i32.to_ne_bytes());
        expected.extend(200u32.to_ne_bytes());
        assert_eq!(ConfigureWindow::OPCODE, 12);
        assert_eq!(mask.bits(), 5);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn create_window_request_bytes() {
        use crate::x11::generated::{
            CreateWindow, CreateWindowAux, Cw, EventMask, Window, WindowClass,
        };

        let mask = Cw::BackPixel | Cw::EventMask;
        let bytes = request_bytes(
            &CreateWindow {
                depth: 24,
                wid: Window(10),
                parent: Window(1),
                x: 0,
                y: 0,
                width: 100,
                height: 80,
                border_width: 1,
                class: WindowClass::InputOutput,
                visual: 0x21,
                value_list: CreateWindowAux::default()
                    .background_pixel(0x00ff_ffff)
                    .event_mask(EventMask::KeyPress),
            },
            0,
        );
        let mut expected = vec![CreateWindow::OPCODE, 24];
        expected.extend(10u16.to_ne_bytes());
        expected.extend(10u32.to_ne_bytes());
        expected.extend(1u32.to_ne_bytes());
        expected.extend(0i16.to_ne_bytes());
        expected.extend(0i16.to_ne_bytes());
        expected.extend(100u16.to_ne_bytes());
        expected.extend(80u16.to_ne_bytes());
        expected.extend(1u16.to_ne_bytes());
        expected.extend((WindowClass::InputOutput.0 as u16).to_ne_bytes());
        expected.extend(0x21u32.to_ne_bytes());
        expected.extend(mask.bits().to_ne_bytes());
        expected.extend(0x00ff_ffffu32.to_ne_bytes());
        expected.extend(EventMask::KeyPress.bits().to_ne_bytes());
        assert_eq!(CreateWindow::OPCODE, 1);
        assert_eq!(mask.bits(), 2050);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn send_event_request_bytes() {
        use crate::x11::generated::{EventMask, SendEvent, Window};

        let event = [0xabu8; 32];
        let bytes = request_bytes(
            &SendEvent {
                propagate: 0,
                destination: Window(4),
                event_mask: EventMask::StructureNotify,
                event: &event,
            },
            0,
        );
        let mut expected = vec![SendEvent::OPCODE, 0];
        expected.extend(11u16.to_ne_bytes());
        expected.extend(4u32.to_ne_bytes());
        expected.extend(EventMask::StructureNotify.bits().to_ne_bytes());
        expected.extend_from_slice(&event);
        assert_eq!(SendEvent::OPCODE, 25);
        assert_eq!(EventMask::StructureNotify.bits(), 1 << 17);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn composite_redirect_subwindows_request_bytes() {
        use crate::x11::generated::{CompositeRedirectSubwindows, ExtensionName, Redirect, Window};

        let bytes = request_bytes(
            &CompositeRedirectSubwindows { window: Window(3), update: Redirect::Manual },
            142,
        );
        let mut expected = vec![142, CompositeRedirectSubwindows::OPCODE];
        expected.extend(3u16.to_ne_bytes());
        expected.extend(3u32.to_ne_bytes());
        expected.push(Redirect::Manual.0 as u8);
        expected.extend_from_slice(&[0, 0, 0]);
        assert_eq!(CompositeRedirectSubwindows::EXTENSION, Some(ExtensionName::Composite));
        assert_eq!(CompositeRedirectSubwindows::OPCODE, 2);
        assert_eq!(Redirect::Manual.0, 1);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn get_property_reply_round_trip() {
        use crate::x11::{
            generated::{Atom, GetPropertyReply},
            wire::{Reader, Writer},
        };

        let reply = GetPropertyReply {
            sequence: 4,
            format: 8,
            r#type: Atom::STRING,
            bytes_after: 0,
            value_len: 3,
            value: b"abc".to_vec(),
        };
        let mut bytes = Vec::new();
        reply.serialize(&mut Writer::new(&mut bytes)).unwrap();
        let mut expected = vec![1, reply.format];
        expected.extend(reply.sequence.to_ne_bytes());
        expected.extend(1u32.to_ne_bytes());
        expected.extend(Atom::STRING.0.to_ne_bytes());
        expected.extend(0u32.to_ne_bytes());
        expected.extend(3u32.to_ne_bytes());
        expected.extend_from_slice(&[0; 12]);
        expected.extend_from_slice(b"abc");
        expected.push(0);
        assert_eq!(bytes, expected);

        let mut reader = Reader::new(&bytes);
        assert_eq!(GetPropertyReply::parse(&mut reader).unwrap(), reply);
        assert_eq!(reader.remaining(), 0);
    }

    fn event_bytes(event: &crate::x11::generated::Event) -> Vec<u8> {
        let mut bytes = Vec::new();
        event
            .serialize(&mut crate::x11::wire::Writer::new(&mut bytes))
            .unwrap();
        bytes
    }

    #[test]
    fn client_message_round_trip() {
        use crate::x11::{
            generated::{Atom, ClientMessage, ClientMessageData, Event, Window},
            wire::Reader,
        };

        let mut data = ClientMessageData::default();
        data.set_data32([1, 2, 3, 4, 5]);
        let message = ClientMessage {
            send_event: true,
            sequence: 9,
            format: 32,
            window: Window(7),
            r#type: Atom::WM_NAME,
            data,
        };
        let bytes = event_bytes(&Event::ClientMessage(message));
        let mut expected = vec![ClientMessage::CODE | 0x80, 32];
        expected.extend(9u16.to_ne_bytes());
        expected.extend(7u32.to_ne_bytes());
        expected.extend(Atom::WM_NAME.0.to_ne_bytes());
        for value in [1u32, 2, 3, 4, 5] {
            expected.extend(value.to_ne_bytes());
        }
        assert_eq!(ClientMessage::CODE, 33);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes, expected);

        let mut reader = Reader::new(&bytes);
        assert_eq!(ClientMessage::parse(&mut reader).unwrap(), message);
        assert_eq!(reader.remaining(), 0);
        assert_eq!(Event::parse(&bytes, &[]).unwrap(), Event::ClientMessage(message));
        assert!(Event::parse(&bytes, &[]).unwrap().send_event());

        let mut longer = bytes.clone();
        longer.push(0);
        assert!(matches!(
            Event::parse(&longer, &[]),
            Err(crate::x11::wire::WireError::RequestTooLong)
        ));
    }

    #[test]
    fn configure_request_round_trip() {
        use crate::x11::generated::{ConfigWindow, ConfigureRequest, Event, StackMode, Window};

        let request = ConfigureRequest {
            send_event: false,
            sequence: 4,
            stack_mode: StackMode::Above,
            parent: Window(1),
            window: Window(2),
            sibling: Window(3),
            x: -4,
            y: 5,
            width: 6,
            height: 7,
            border_width: 8,
            value_mask: ConfigWindow::X | ConfigWindow::Width,
        };
        let bytes = event_bytes(&Event::ConfigureRequest(request));
        let mut expected = vec![ConfigureRequest::CODE, StackMode::Above.0 as u8];
        expected.extend(4u16.to_ne_bytes());
        expected.extend(1u32.to_ne_bytes());
        expected.extend(2u32.to_ne_bytes());
        expected.extend(3u32.to_ne_bytes());
        expected.extend((-4i16).to_ne_bytes());
        expected.extend(5i16.to_ne_bytes());
        expected.extend(6u16.to_ne_bytes());
        expected.extend(7u16.to_ne_bytes());
        expected.extend(8u16.to_ne_bytes());
        expected.extend(((ConfigWindow::X | ConfigWindow::Width).bits() as u16).to_ne_bytes());
        expected.extend_from_slice(&[0; 4]);
        assert_eq!(ConfigureRequest::CODE, 23);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes, expected);
        assert_eq!(Event::parse(&bytes, &[]).unwrap(), Event::ConfigureRequest(request));
    }

    #[test]
    fn property_notify_round_trip() {
        use crate::x11::generated::{Atom, Event, Property, PropertyNotify, Window};

        let notify = PropertyNotify {
            send_event: false,
            sequence: 11,
            window: Window(8),
            atom: Atom::WM_NAME,
            time: 12,
            state: Property::Delete,
        };
        let bytes = event_bytes(&Event::PropertyNotify(notify));
        let mut expected = vec![PropertyNotify::CODE, 0];
        expected.extend(11u16.to_ne_bytes());
        expected.extend(8u32.to_ne_bytes());
        expected.extend(Atom::WM_NAME.0.to_ne_bytes());
        expected.extend(12u32.to_ne_bytes());
        expected.push(Property::Delete.0 as u8);
        expected.extend_from_slice(&[0; 15]);
        assert_eq!(PropertyNotify::CODE, 28);
        assert_eq!(Property::NewValue.0, 0);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes, expected);
        assert_eq!(Event::parse(&bytes, &[]).unwrap(), Event::PropertyNotify(notify));
    }

    #[test]
    fn xfixes_selection_notify_round_trip() {
        use crate::x11::generated::{
            Atom, Event, ExtensionEvent, ExtensionName, SelectionEvent, Window,
            XFixesSelectionNotify,
        };

        let notify = XFixesSelectionNotify {
            send_event: false,
            code: 80,
            sequence: 6,
            subtype: SelectionEvent::SetSelectionOwner,
            window: Window(2),
            owner: Window(3),
            selection: Atom::PRIMARY,
            timestamp: 4,
            selection_timestamp: 5,
        };
        let bytes = event_bytes(&Event::XFixesSelectionNotify(notify));
        let mut expected = vec![80, SelectionEvent::SetSelectionOwner.0 as u8];
        expected.extend(6u16.to_ne_bytes());
        expected.extend(2u32.to_ne_bytes());
        expected.extend(3u32.to_ne_bytes());
        expected.extend(Atom::PRIMARY.0.to_ne_bytes());
        expected.extend(4u32.to_ne_bytes());
        expected.extend(5u32.to_ne_bytes());
        expected.extend_from_slice(&[0; 8]);
        assert_eq!(XFixesSelectionNotify::NUMBER, 0);
        assert_eq!(SelectionEvent::SetSelectionOwner.0, 0);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes, expected);
        let extensions = [ExtensionEvent { extension: ExtensionName::XFixes, first_event: 80 }];
        assert_eq!(
            Event::parse(&bytes, &extensions).unwrap(),
            Event::XFixesSelectionNotify(notify)
        );
        assert!(matches!(Event::parse(&bytes, &[]).unwrap(), Event::Unknown { code: 80, .. }));
    }

    #[test]
    fn unknown_event_keeps_the_send_event_bit() {
        use crate::x11::generated::Event;

        let mut raw = vec![50 | 0x80];
        raw.extend_from_slice(&[1; 31]);
        let Event::Unknown { send_event, code, bytes } = Event::parse(&raw, &[]).unwrap() else {
            panic!("code 50 is not a selected event");
        };
        assert!(send_event);
        assert_eq!(code, 50);
        assert_eq!(bytes[0], 50);
        assert_eq!(event_bytes(&Event::Unknown { send_event, code, bytes }), raw);

        let xge = vec![35, 0, 0, 0, 1, 0, 0, 0];
        let Event::Unknown { send_event, code, bytes } = Event::parse(&xge, &[]).unwrap() else {
            panic!("code 35 stays unknown");
        };
        assert!(!send_event);
        assert_eq!(code, 35);
        assert_eq!(bytes, xge);
    }

    #[test]
    fn xerror_round_trip_and_names() {
        use crate::x11::{
            generated::{error_name, ExtensionError, ExtensionName, XError},
            wire::{Reader, Writer},
        };

        let error = XError {
            code: 3,
            sequence: 9,
            bad_value: 4,
            minor_opcode: 5,
            major_opcode: 6,
        };
        let mut bytes = Vec::new();
        error.serialize(&mut Writer::new(&mut bytes)).unwrap();
        let mut expected = vec![0, 3];
        expected.extend(9u16.to_ne_bytes());
        expected.extend(4u32.to_ne_bytes());
        expected.extend(5u16.to_ne_bytes());
        expected.push(6);
        expected.extend_from_slice(&[0; 21]);
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes, expected);
        let mut reader = Reader::new(&bytes);
        assert_eq!(XError::parse(&mut reader).unwrap(), error);
        assert_eq!(reader.remaining(), 0);

        assert_eq!(error_name(3, &[]), Some("Window"));
        assert_eq!(error_name(1, &[]), Some("Request"));
        assert_eq!(
            error_name(
                128,
                &[ExtensionError { extension: ExtensionName::XFixes, first_error: 128 }]
            ),
            Some("BadRegion")
        );
        assert_eq!(
            error_name(
                129,
                &[ExtensionError { extension: ExtensionName::Render, first_error: 128 }]
            ),
            Some("Picture")
        );
        assert_eq!(error_name(40, &[]), None);
    }
}
