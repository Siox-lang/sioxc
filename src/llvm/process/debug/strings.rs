//! Borrowed runtime string contents, not a fictitious inline Char[] layout.

use super::*;

pub(super) struct StringMirror<'ctx> {
    source: GlobalValue<'ctx>,
    view: GlobalValue<'ctx>,
    root: u32,
}

impl<'ctx> DebugInfo<'ctx, '_> {
    fn string_view_type(&self) -> inkwell::types::StructType<'ctx> {
        let integer = self.context.i64_type();
        let pointer = self.context.ptr_type(AddressSpace::default());
        self.context.struct_type(
            &[
                integer.into(),
                pointer.into(),
                integer.into(),
                pointer.into(),
            ],
            false,
        )
    }

    pub(super) fn string_debug_type(&self, scope: DIScope<'ctx>, span: Span) -> DIType<'ctx> {
        let view_type = self.string_view_type();
        let pointer_bits = u64::from(self.target.get_pointer_byte_size(None)) * 8;
        let pointer = |name, pointee| {
            self.builder
                .create_pointer_type(name, pointee, pointer_bits, 0, AddressSpace::default())
                .as_type()
        };
        let file = self.file(span.file);
        let line = self.sources.line_col(span.file, span.start).0;
        let integer = self.basic("length", 64, DW_ATE_UNSIGNED);
        let fields = [
            ("length", integer, 64),
            (
                "data",
                pointer("Char*", self.basic("Char", 32, DW_ATE_UNSIGNED)),
                pointer_bits,
            ),
            ("byte_length", integer, 64),
            (
                "utf8",
                pointer(
                    "UTF-8*",
                    self.basic("char", 8, 0x08 /* DW_ATE_unsigned_char */),
                ),
                pointer_bits,
            ),
        ];
        let members = fields
            .iter()
            .enumerate()
            .map(|(index, (name, ty, bits))| {
                self.builder
                    .create_member_type(
                        scope,
                        name,
                        file,
                        line,
                        *bits,
                        0,
                        self.target
                            .offset_of_element(&view_type, index as u32)
                            .unwrap()
                            * 8,
                        DIFlags::ZERO,
                        *ty,
                    )
                    .as_type()
            })
            .collect::<Vec<_>>();
        let bits = self.target.get_store_size(&view_type) * 8;
        self.builder
            .create_struct_type(
                scope,
                "Char[]",
                file,
                line,
                bits,
                0,
                DIFlags::ZERO,
                None,
                &members,
                0,
                None,
                "",
            )
            .as_type()
    }

    pub(super) fn string_mirror(&self, module: &Module<'ctx>, source: &str, root: u32) -> String {
        let buffer_name = format!("{source}.string.debug");
        if module.get_global(&buffer_name).is_some() {
            return buffer_name;
        }
        let view_type = self.string_view_type();
        let bits = self.target.get_store_size(&view_type) * 8;
        let buffer = self.inspection_buffer(module, &buffer_name, u32::try_from(bits / 8).unwrap());
        buffer.set_alignment(self.target.get_abi_alignment(&view_type));
        self.string_mirrors.borrow_mut().push(StringMirror {
            source: module.get_global(source).expect("declared string handle"),
            view: buffer,
            root,
        });
        buffer_name
    }

    pub(super) fn string_constant_view(&self, module: &Module<'ctx>, name: &str, text: &str) {
        let pointer = self.context.ptr_type(AddressSpace::default());
        let (characters, utf8) = if text.is_empty() {
            (pointer.const_null(), pointer.const_null())
        } else {
            let characters = text
                .chars()
                .map(|character| {
                    self.context
                        .i32_type()
                        .const_int(u64::from(u32::from(character)), false)
                })
                .collect::<Vec<_>>();
            let data = self.context.i32_type().const_array(&characters);
            let global = module.add_global(data.get_type(), None, &format!("{name}.chars"));
            global.set_initializer(&data);
            global.set_constant(true);
            global.set_linkage(Linkage::Private);
            (
                global.as_pointer_value(),
                private_string(self.context, module, &format!("{name}.utf8"), text),
            )
        };
        let integer = self.context.i64_type();
        let view = self.string_view_type().const_named_struct(&[
            integer.const_int(text.chars().count() as u64, false).into(),
            characters.into(),
            integer.const_int(text.len() as u64, false).into(),
            utf8.into(),
        ]);
        let global = module.add_global(view.get_type(), None, name);
        global.set_initializer(&view);
        global.set_constant(true);
        global.set_linkage(Linkage::Private);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn string_global(
        &self,
        module: &Module<'ctx>,
        source: &str,
        scope: DIScope<'ctx>,
        name: &str,
        span: Span,
        root: u32,
    ) {
        let ty = self.string_debug_type(scope, span);
        let file = self.file(span.file);
        let line = self.sources.line_col(span.file, span.start).0;
        let buffer_name = self.string_mirror(module, source, root);
        let info = self.builder.create_global_variable_expression(
            scope, name, "", file, line, ty, true, None, None, 0,
        );
        self.attach_global(module, &buffer_name, info);
    }

    pub(super) fn refresh_strings(&self, module: &Module<'ctx>, builder: &Builder<'ctx>) {
        if self.string_mirrors.borrow().is_empty() {
            return;
        }
        let pointer = self.context.ptr_type(AddressSpace::default());
        let function_type = self.context.void_type().fn_type(
            &[
                self.context.i32_type().into(),
                self.context.i64_type().into(),
                pointer.into(),
                pointer.into(),
                pointer.into(),
                pointer.into(),
            ],
            false,
        );
        let query = module.add_function("sx_runtime_debug_string", function_type, None);
        let view_type = self.string_view_type();
        for mirror in self.string_mirrors.borrow().iter() {
            let handle = builder
                .build_load(
                    self.context.i64_type(),
                    mirror.source.as_pointer_value(),
                    "debug.string.handle",
                )
                .unwrap();
            let mut arguments = vec![
                self.context
                    .i32_type()
                    .const_int(u64::from(mirror.root), false)
                    .into(),
                handle.into(),
            ];
            for index in 0..4 {
                arguments.push(
                    builder
                        .build_struct_gep(
                            view_type,
                            mirror.view.as_pointer_value(),
                            index,
                            "debug.string.field",
                        )
                        .unwrap()
                        .into(),
                );
            }
            builder.build_call(query, &arguments, "").unwrap();
        }
    }
}
