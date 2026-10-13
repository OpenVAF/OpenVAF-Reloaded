//! The descriptor's `accept`: what a module does when a timepoint it was evaluated
//! at turns out to be the one the simulator kept.
//!
//! The work itself is the runtime's (`openvaf/osdi/stdlib.c`), but where a module's
//! retained state lives is known only here, so what the descriptor points at is a
//! function generated per module: it hands the runtime that instance's retained
//! slots and then lets it do the rest. See `docs/osdi-timepoint-protocol.md`.

use core::ptr::NonNull;

use hir_lower::{CallBackKind, FileOp};
use llvm_sys::core::{
    LLVMAppendBasicBlockInContext, LLVMBuildCall2, LLVMCreateBuilderInContext, LLVMDisposeBuilder,
    LLVMGetParam, LLVMPositionBuilderAtEnd,
};
use mir_llvm::UNNAMED;

use crate::compilation_unit::OsdiCompilationUnit;

impl<'ll> OsdiCompilationUnit<'_, '_, 'll> {
    /// Whether this module has anything to do at an accepted timepoint: a line to
    /// write (VAMS-2023 9.4.1), records a point consumed (9.5.4), or a value
    /// retained from one timestep to the next (4.5.10), which is final exactly when
    /// the point it was computed at is.
    pub fn has_accept_work(&self) -> bool {
        self.inst_data.num_retained != 0
            || self.module.intern.callbacks.raw.iter().any(|cb| {
                matches!(cb, CallBackKind::StrobeLine { .. } | CallBackKind::File(FileOp::Gets))
            })
    }

    /// `void accept(void *handle, void *inst, void *model, OsdiSimInfo *info)`.
    pub fn accept(&self) -> &'ll llvm_sys::LLVMValue {
        let OsdiCompilationUnit { cx, module, inst_data, .. } = self;
        let void_ptr = cx.ty_ptr();
        let fun_ty = cx.ty_func(&[void_ptr, void_ptr, void_ptr, void_ptr], cx.ty_void());
        let name = &format!("accept_{}", module.sym);
        let llfunc = cx.declare_int_c_fn(name, fun_ty);

        unsafe {
            let entry = LLVMAppendBasicBlockInContext(
                NonNull::from(cx.llcx).as_ptr(),
                NonNull::from(llfunc).as_ptr(),
                UNNAMED,
            );
            let llbuilder = LLVMCreateBuilderInContext(NonNull::from(cx.llcx).as_ptr());
            LLVMPositionBuilderAtEnd(llbuilder, entry);

            let handle = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 0);
            let instance = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 1);
            let model = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 2);
            let info = &*LLVMGetParam(NonNull::from(llfunc).as_ptr(), 3);

            // The values retained from this timestep are the values retained from
            // an accepted one, which is what the model reads them back as. Nothing
            // else knows where they are.
            if inst_data.num_retained != 0 {
                let vals = inst_data.retained_vals_ptr(cx, instance, &*llbuilder);
                let num = cx.const_unsigned_int(inst_data.num_retained);
                let fun = cx
                    .get_func_by_name("commit_retained_now")
                    .expect("stdlib function commit_retained_now is missing");
                let fun_ty = cx.ty_func(&[cx.ty_ptr(), cx.ty_int()], cx.ty_void());
                let mut args = [vals, num];
                LLVMBuildCall2(
                    llbuilder,
                    NonNull::from(fun_ty).as_ptr(),
                    NonNull::from(fun).as_ptr(),
                    args.as_mut_ptr() as *mut _,
                    2,
                    UNNAMED,
                );
            }

            // The lines this instance composed at the point, and the records its
            // reads consumed.
            let fun =
                cx.get_func_by_name("va_accept").expect("stdlib function va_accept is missing");
            let fun_ty = cx.ty_func(&[void_ptr, void_ptr, void_ptr, void_ptr], cx.ty_void());
            let mut args = [handle, instance, model, info];
            LLVMBuildCall2(
                llbuilder,
                NonNull::from(fun_ty).as_ptr(),
                NonNull::from(fun).as_ptr(),
                args.as_mut_ptr() as *mut _,
                4,
                UNNAMED,
            );

            llvm_sys::core::LLVMBuildRetVoid(llbuilder);
            LLVMDisposeBuilder(llbuilder);
        }

        llfunc
    }
}
