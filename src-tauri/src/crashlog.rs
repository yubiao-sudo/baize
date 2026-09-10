//! 进程级崩溃诊断：未处理异常（AV 段错误等）时把异常码/地址/RIP 写入日志文件。
//!
//! 背景：GUI 自动化曾观测到 click_element 间歇性 Segmentation fault（约 2/6 次），
//! 无法稳定复现，Windows 事件日志也无 WER 记录。本模块在进程初始化时装上
//! 最后一道异常过滤器，崩溃时写 `%TEMP%\baize-crash-<pid>.log`，下次发生即可
//! 拿到异常码与出错地址，再结合地址定位模块。只记录不吞异常（返回 CONTINUE_SEARCH），
//! 不改变任何原有崩溃行为。
//!
//! 结构体手工声明（与 Win64 ABI 对齐），避免为此引入 windows crate 的
//! Win32_System_Diagnostics_Debug feature。RIP 在 x64 CONTEXT 中偏移 0xF8。

use std::sync::atomic::{AtomicBool, Ordering};

static INSTALLED: AtomicBool = AtomicBool::new(false);

#[repr(C)]
struct EXCEPTION_RECORD64 {
    exception_code: u32,
    exception_flags: u32,
    exception_record: u64,
    exception_address: u64,
    number_parameters: u32,
    __unused_alignment: u32,
    exception_information: [u64; 15],
}

#[repr(C)]
struct EXCEPTION_POINTERS {
    exception_record: *mut EXCEPTION_RECORD64,
    context_record: *mut u8, // x64 CONTEXT，仅按偏移取 RIP
}

type ExceptionFilter = Option<unsafe extern "system" fn(*mut EXCEPTION_POINTERS) -> i32>;

#[link(name = "kernel32")]
extern "system" {
    fn SetUnhandledExceptionFilter(filter: ExceptionFilter) -> isize;
}

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

unsafe extern "system" fn top_level_filter(ep: *mut EXCEPTION_POINTERS) -> i32 {
    unsafe {
        if ep.is_null() || (*ep).exception_record.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let rec = &*(*ep).exception_record;
        let code = rec.exception_code;
        let addr = rec.exception_address;
        // x64 CONTEXT.Rip 偏移 0xF8
        let rip = if (*ep).context_record.is_null() {
            0
        } else {
            (*ep).context_record.add(0xF8).cast::<u64>().read_unaligned()
        };
        let tid = std::thread::current().id();
        let path = std::env::temp_dir().join(format!(
            "baize-crash-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0)
        ));
        let text = format!(
            "baize 未处理异常\nexception_code=0x{code:08X}\nexception_address=0x{addr:016X}\nrip=0x{rip:016X}\nthread_id={tid:?}\n\
             说明：code 0xC0000005=访问违例（段错误）、0xC00000FD=栈溢出、0xC0000409=快速失败；\n\
             可用 `llvm-symbolizer` 或 VS 对 rip 求符号定位崩溃函数。\n"
        );
        let _ = std::fs::write(&path, text);
        EXCEPTION_CONTINUE_SEARCH
    }
}

/// 装上崩溃过滤器（幂等）。在进程启动尽早调用。
pub fn install() {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(windows)]
    unsafe {
        SetUnhandledExceptionFilter(Some(top_level_filter));
    }
}
