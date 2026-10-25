//! `elfq FILE browse`, driven with keys through its event loop on
//! ratatui's TestBackend, a screen in memory: what each screen shows, at
//! 80x24 and 100x30. Errors that come before the screen, such as needing a
//! terminal, are tested on the elfq program, as other commands' are.

use std::cell::Cell;
use std::path::Path;

use elfq::binary::Binary;
use elfq::commands::browse::{App, Session, Stop, event_loop};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::damaged;
use crate::support::{SAMPLE, assert_output, elfq, load_sample, sample_path};

/// A key as a test script writes it: a name such as `Enter`, `Tab`, `Esc`
/// or `Ctrl-O`, or text, typed a character at a time.
fn keys(script: &[&str]) -> Vec<Event> {
    let mut events = Vec::new();
    for &word in script {
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let code = match word {
            "Enter" => KeyCode::Enter,
            "Tab" => KeyCode::Tab,
            "BackTab" => KeyCode::BackTab,
            "Esc" => KeyCode::Esc,
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "PageUp" => KeyCode::PageUp,
            "PageDown" => KeyCode::PageDown,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "Backspace" => KeyCode::Backspace,
            _ => {
                if let Some(letter) = word.strip_prefix("Ctrl-") {
                    let letter = letter.to_lowercase().chars().next().unwrap();
                    events.push(Event::Key(KeyEvent::new(
                        KeyCode::Char(letter),
                        KeyModifiers::CONTROL,
                    )));
                } else {
                    events.extend(word.chars().map(|c| Event::Key(key(KeyCode::Char(c)))));
                }
                continue;
            }
        };
        events.push(Event::Key(key(code)));
    }
    events
}

/// The browser on `binary`, loaded from `path`, opened at `symbol` on a
/// screen of `width` by `height`, after `script`'s keys: the screen as
/// text, with the spaces at the ends of lines left out, and why the event
/// loop stopped.
fn browse(
    path: &str,
    binary: &Binary,
    symbol: Option<&str>,
    (width, height): (u16, u16),
    script: &[&str],
) -> (String, Stop) {
    let session = Session::new(Path::new(path), binary).unwrap();
    let mut app = App::new(session, symbol).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let mut events = keys(script).into_iter();
    let stop = event_loop(&mut terminal, &mut app, || Ok(events.next()), || Ok(false)).unwrap();
    let buffer = terminal.backend().buffer();
    let mut screen = String::new();
    for row in 0..height {
        let line: String = (0..width)
            .map(|column| buffer[(column, row)].symbol())
            .collect();
        screen += line.trim_end();
        screen.push('\n');
    }
    (screen, stop)
}

/// The screen of the browser on `sample`, as [`browse`] gives it.
fn screen(sample: &str, symbol: Option<&str>, size: (u16, u16), script: &[&str]) -> String {
    let path = format!("testdata/bin/{sample}");
    browse(&path, &load_sample(sample), symbol, size, script).0
}

/// The selected line of `screen`, and its status line, the one before
/// the hints.
fn selected_and_status(screen: &str) -> (&str, &str) {
    let lines: Vec<&str> = screen.lines().collect();
    let selected = lines
        .iter()
        .find(|line| line.starts_with("> "))
        .unwrap_or(&"");
    (selected, lines[lines.len() - 2])
}

#[test]
fn a_function_shows_its_code_and_what_each_line_uses() {
    assert_output(
        &screen("c-x86_64.elf", Some("log_msg"), (100, 30), &[]),
        "\
c-x86_64.elf › log_msg
log_msg  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
  ADDRESS  INSTRUCTION
> 0x1610   endbr64
  0x1614   push    rbx
  0x1615   cmp     byte ptr [rdi], 0
  0x1618   mov     rbx, rdi
  0x161b   je      0x1638             jump ↓
  0x161d   lea     rdi, [rdi+1]
  0x1621   call    strlen@plt         call
  0x1626   mov     rdi, rbx
  0x1629   pop     rbx
  0x162a   lea     rsi, [rax+1]
  0x162e   jmp     platform_write     tail call
  0x1633   nop     [rax+rax]
  0x1638   mov     rdi, rbx
  0x163b   xor     esi, esi
  0x163d   pop     rbx
  0x163e   jmp     platform_write     tail call









↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // Tab goes to the next line that uses another item, and the status
    // line says where its link goes.
    assert_output(
        &screen("c-x86_64.elf", Some("log_msg"), (80, 24), &["Tab"]),
        "\
c-x86_64.elf › log_msg
log_msg  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
  ADDRESS  INSTRUCTION
  0x1610   endbr64
  0x1614   push    rbx
  0x1615   cmp     byte ptr [rdi], 0
  0x1618   mov     rbx, rdi
  0x161b   je      0x1638             jump ↓
  0x161d   lea     rdi, [rdi+1]
> 0x1621   call    strlen@plt         call
  0x1626   mov     rdi, rbx
  0x1629   pop     rbx
  0x162a   lea     rsi, [rax+1]
  0x162e   jmp     platform_write     tail call
  0x1633   nop     [rax+rax]
  0x1638   mov     rdi, rbx
  0x163b   xor     esi, esi
  0x163d   pop     rbx
  0x163e   jmp     platform_write     tail call


→ strlen@plt  function at 0x1080, 16 bytes, in .plt.sec
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn enter_follows_a_call_and_esc_comes_back_to_it() {
    let follow = ["Tab", "Tab", "Enter"];
    assert_output(
        &screen("c-x86_64.elf", Some("log_msg"), (80, 24), &follow),
        "\
c-x86_64.elf › log_msg › platform_write
platform_write  function at 0x1ac0, 20 bytes, in .text
used by 1 · uses 1
  ADDRESS  INSTRUCTION
> 0x1ac0   endbr64
  0x1ac4   mov     rdx, rsi
  0x1ac7   mov     rsi, rdi
  0x1aca   mov     edi, 1
  0x1acf   jmp     write@plt  tail call














↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // Back on the line that was followed, and forward again.
    let back = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &[&follow[..], &["Esc"]].concat(),
    );
    assert_eq!(
        selected_and_status(&back),
        (
            "> 0x162e   jmp     platform_write     tail call",
            "→ platform_write  function at 0x1ac0, 20 bytes, in .text"
        )
    );
    for key in ["Left", "h", "Backspace", "Ctrl-O"] {
        let other = screen(
            "c-x86_64.elf",
            Some("log_msg"),
            (80, 24),
            &[&follow[..], &[key]].concat(),
        );
        assert_eq!(
            selected_and_status(&other),
            selected_and_status(&back),
            "{key}"
        );
    }
    let forward = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &[&follow[..], &["Esc", "]"]].concat(),
    );
    assert!(
        forward.starts_with("c-x86_64.elf › log_msg › platform_write\n"),
        "{forward}"
    );
}

#[test]
fn a_jump_inside_the_function_moves_the_cursor_and_esc_brings_it_back() {
    // `je 0x1638`, then where it goes.
    let to_je = ["Down", "Down", "Down", "Down"];
    let jumped = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &[&to_je[..], &["Enter"]].concat(),
    );
    assert_eq!(
        selected_and_status(&jumped),
        ("> 0x1638   mov     rdi, rbx", "")
    );
    assert!(jumped.starts_with("c-x86_64.elf › log_msg\n"), "{jumped}");
    let back = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &[&to_je[..], &["Enter", "Esc"]].concat(),
    );
    assert_eq!(
        selected_and_status(&back),
        (
            "> 0x161b   je      0x1638             jump ↓",
            "→ 0x1638, in this function"
        )
    );
    // Then there is nothing to go back to.
    let first = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &[&to_je[..], &["Enter", "Esc", "Esc"]].concat(),
    );
    assert_eq!(
        selected_and_status(&first).1,
        "this is the first page; q quits"
    );
}

#[test]
fn without_symbol_the_function_that_the_overview_starts_from_opens() {
    assert_output(
        &screen("c-x86_64.elf", None, (80, 24), &[]),
        "\
c-x86_64.elf › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
  ADDRESS  INSTRUCTION
> 0x10a0   endbr64
  0x10a4   sub     rsp, 8
  0x10a8   lea     rsi, [rip+0x1b1]  address taken on_timer
  0x10af   xor     edi, edi
  0x10b1   call    register_handler  call
  0x10b6   lea     rsi, [rip+0x593]  address taken on_button
  0x10bd   mov     edi, 1
  0x10c2   call    register_handler  call
  0x10c7   lea     rsi, [rip+0x1a2]  address taken on_data
  0x10ce   mov     edi, 2
  0x10d3   call    register_handler  call
  0x10d8   xor     esi, esi
  0x10da   lea     rdi, [rip+0xf7d]  uses .rodata@0x205e \"[1,[2,3],[4,[5]]]\"
  0x10e1   call    parse_value       call
  0x10e6   mov     esi, 0x19
  0x10eb   lea     rdi, [rip+0xfbe]  uses frame_a
  0x10f2   call    process_frame     call
  0x10f7   mov     esi, 0x11

↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn symbol_is_found_as_the_commands_find_it() {
    // At an address, the line that holds it is selected, and the header
    // says so, as the commands' first line does.
    assert_output(
        &screen("c-x86_64.elf", Some("0x162e"), (80, 24), &[]),
        "\
c-x86_64.elf › log_msg
log_msg (which holds 0x162e)  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
  ADDRESS  INSTRUCTION
  0x1610   endbr64
  0x1614   push    rbx
  0x1615   cmp     byte ptr [rdi], 0
  0x1618   mov     rbx, rdi
  0x161b   je      0x1638             jump ↓
  0x161d   lea     rdi, [rdi+1]
  0x1621   call    strlen@plt         call
  0x1626   mov     rdi, rbx
  0x1629   pop     rbx
  0x162a   lea     rsi, [rax+1]
> 0x162e   jmp     platform_write     tail call
  0x1633   nop     [rax+rax]
  0x1638   mov     rdi, rbx
  0x163b   xor     esi, esi
  0x163d   pop     rbx
  0x163e   jmp     platform_write     tail call


→ platform_write  function at 0x1ac0, 20 bytes, in .text
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // An import has no page: its code is in a shared library.
    assert_output(
        &screen("rust-x86_64-nodebug.elf", Some("memcpy"), (80, 24), &[]),
        "\
rust-x86_64-nodebug.elf › main
main  function at 0x15220, 46 bytes, in .text
used by 1 · uses 4
  ADDRESS  INSTRUCTION
> 0x15220  push    rax
  0x15221  mov     rcx, rsi
  0x15224  movzx   eax, byte ptr [rip-0xade3]  uses __rustc_debug_gdb_scripts_s…
  0x1522b  movsxd  rdx, edi
  0x1522e  lea     rax, [rip-0x695]            address taken elfq_sample::main
  0x15235  mov     [rsp], rax
  0x15239  lea     rsi, [rip+0x3dab8]          uses .data.rel.ro@0x52cf8
  0x15240  mov     rdi, rsp
  0x15243  xor     r8d, r8d
  0x15246  call    qword ptr [rip+0x3f924]     call lang_start_internal
  0x1524c  pop     rcx
  0x1524d  ret






memcpy is imported from a shared library: its code isn't in this file
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_symbol_that_finds_several_items_or_none_opens_go_to() {
    // On the page that opens without SYMBOL, with the CLI's message over
    // the rows.
    assert_output(
        &screen("cpp-x86_64.elf", Some("log"), (80, 24), &[]),
        "\
cpp-x86_64.elf › main › go to
`log` could mean 2 functions
  ADDRESS  SIZE  KIND      SECTION  ITEM
> 0x13c0     64  function  .text    app::log(char const*)
  0x1400    182  function  .text    app::log(unsigned int)
  0x11f0     16  function  .text    _GLOBAL__sub_I__ZN3app3logEPKc
















go to: log▏
↑↓ move  enter open  esc cancel
",
    );
    let unknown = screen("c-x86_64.elf", Some("nosuch"), (80, 24), &[]);
    let lines: Vec<&str> = unknown.lines().collect();
    assert_eq!(lines[1], "no function or data object is named `nosuch`");
    assert_eq!(lines[22], "go to: nosuch▏");
    // The message goes when the text changes, and the one for the new
    // text shows under the header; Esc shows the page.
    let typed = screen("c-x86_64.elf", Some("nosuch"), (80, 24), &["Backspace"]);
    let lines: Vec<&str> = typed.lines().collect();
    assert_eq!(
        lines[1..3],
        [
            "  ADDRESS  SIZE  KIND  SECTION  ITEM",
            "no function or data object is named `nosuc`"
        ]
    );
    let page = screen("c-x86_64.elf", Some("nosuch"), (80, 24), &["Esc"]);
    assert!(page.starts_with("c-x86_64.elf › main\n"), "{page}");
}

#[test]
fn g_goes_to_what_is_typed() {
    // Names that have the text, largest first, after what SYMBOL finds.
    assert_output(
        &screen("c-cortex-m7.elf", None, (100, 16), &["g", "crc"]),
        "\
c-cortex-m7.elf › main › go to
  ADDRESS     SIZE  KIND      SECTION  ITEM
> 0x20000000  1024  data      .data    crc_table
  0x10          48  function  .itcm    crc32
  0x300009b0     8  function  .text    __crc32_veneer









go to: crc▏
↑↓ move  enter open  esc cancel
",
    );
    let opened = screen(
        "c-cortex-m7.elf",
        None,
        (100, 16),
        &["g", "crc", "Down", "Enter"],
    );
    assert!(
        opened.starts_with("c-cortex-m7.elf › main › crc32\n"),
        "{opened}"
    );
    // An address opens the item that holds it, at that line.
    assert_output(
        &screen("c-x86_64.elf", None, (80, 24), &["g", "0x162e", "Enter"]),
        "\
c-x86_64.elf › main › log_msg
log_msg (which holds 0x162e)  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
  ADDRESS  INSTRUCTION
  0x1610   endbr64
  0x1614   push    rbx
  0x1615   cmp     byte ptr [rdi], 0
  0x1618   mov     rbx, rdi
  0x161b   je      0x1638             jump ↓
  0x161d   lea     rdi, [rdi+1]
  0x1621   call    strlen@plt         call
  0x1626   mov     rdi, rbx
  0x1629   pop     rbx
  0x162a   lea     rsi, [rax+1]
> 0x162e   jmp     platform_write     tail call
  0x1633   nop     [rax+rax]
  0x1638   mov     rdi, rbx
  0x163b   xor     esi, esi
  0x163d   pop     rbx
  0x163e   jmp     platform_write     tail call


→ platform_write  function at 0x1ac0, 20 bytes, in .text
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // Keys type, but for Ctrl-C and Ctrl-Z, and Esc cancels.
    let typed = screen("c-x86_64.elf", None, (80, 24), &["g", "q?j", "Ctrl-O"]);
    assert_eq!(typed.lines().nth(22), Some("go to: q?j▏"));
    let cancelled = screen("c-x86_64.elf", None, (80, 24), &["g", "log", "Esc"]);
    assert!(
        cancelled.starts_with("c-x86_64.elf › main\n"),
        "{cancelled}"
    );
    let binary = load_sample("c-x86_64.elf");
    let stop = |script: &[&str]| browse(SAMPLE, &binary, None, (80, 24), script).1;
    assert_eq!(stop(&["g", "Ctrl-C"]), Stop::Quit);
    #[cfg(unix)]
    assert_eq!(stop(&["g", "Ctrl-Z"]), Stop::Suspend);
    // With nothing typed, the box says what to type.
    let empty = screen("c-x86_64.elf", None, (80, 24), &["g"]);
    assert_eq!(
        empty.lines().nth(1),
        Some("type a name, the end of a name, or an address")
    );
}

#[test]
fn question_mark_opens_the_help() {
    assert_output(
        &screen("c-cortex-m7.elf", Some("app_tick"), (100, 34), &["?"]),
        "\
c-cortex-m7.elf › app_tick › help
Keys
  ↑ ↓  k j             move
  PgUp PgDn Space      a page up or down
  Home End             the first or last line
  Enter → l            follow the line's link
  Esc ← h Bksp Ctrl-O  back to where you were
  ]                    forward again
  Tab Shift-Tab        the next or previous line that uses another item
  g                    go to a function or data object, by name or address
  ?                    this help
  Ctrl-Z               back to the shell; fg returns
  q Ctrl-C             quit
Words
  call                 a direct call, or a call through a register that the code loaded with the
                       target's address, as a long call does
  tail call            a jump to the start of a function, which then returns to the caller's caller
  uses                 code reads or writes data, or loads its address
  address taken        code loads a function's address, as when passing a function pointer, or an
                       import's
  pointer              data holds a pointer to it, as in a table of functions
  via veneer           through a stub that the linker added; the note names where it ends up
  via PLT              through a PLT stub that passes the call on to one of the file's own
                       functions; the note names it
  jump ↓  jump ↑       a branch to a line further down or up the function, or a case of a table of
                       jumps; Enter moves there
  address ↓  address ↑ adr loads the address of a line in this function, such as a table's; Enter
                       moves there
  = 0x028f5c28         the constant that a load from a literal pool reads
  in .bss              the section of an item in another block of memory than this function
  ~12  ?               an estimated size, and an unknown size


↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    // It scrolls, and Esc goes back.
    let end = screen("c-x86_64.elf", None, (60, 16), &["?", "End"]);
    assert!(
        end.contains("\n  ~12  ?               an estimated size, and an unknown\n"),
        "{end}"
    );
    assert!(
        end.contains("\n  address ↓  address ↑ adr loads the address of a line in\n"),
        "{end}"
    );
    let back = screen("c-x86_64.elf", None, (80, 24), &["?", "Esc"]);
    assert!(back.starts_with("c-x86_64.elf › main\n"), "{back}");
}

#[test]
fn a_shared_library_calls_its_own_functions_through_plt_stubs() {
    assert_output(
        &screen("c-x86_64.so", Some("app_tick"), (100, 30), &[]),
        "\
c-x86_64.so › app_tick
app_tick  function at 0x1770, 62 bytes, in .text
used by 1 · uses 3
  ADDRESS  INSTRUCTION
> 0x1770   endbr64
  0x1774   imul    eax, edi, 0xc28f5c29
  0x177a   push    rbx
  0x177b   mov     ebx, edi
  0x177d   ror     eax, 2
  0x1780   cmp     eax, 0x28f5c28
  0x1785   jbe     0x1798                jump ↓
  0x1787   mov     esi, ebx
  0x1789   xor     edi, edi
  0x178b   pop     rbx
  0x178c   jmp     dispatch_event@plt    tail call via PLT → dispatch_event
  0x1791   nop     [rax]
  0x1798   lea     rdi, [rip+0x869]      uses .rodata@0x2008 \"tick\\n\"
  0x179f   call    log_msg@plt           call via PLT → log_msg
  0x17a4   mov     esi, ebx
  0x17a6   xor     edi, edi
  0x17a8   pop     rbx
  0x17a9   jmp     dispatch_event@plt    tail call via PLT → dispatch_event







↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // Through the stub, to the function.
    assert_output(
        &screen(
            "c-x86_64.so",
            Some("app_tick"),
            (100, 30),
            &["Tab", "Tab", "Tab", "Enter"],
        ),
        "\
c-x86_64.so › app_tick › log_msg
log_msg  function at 0x16c0, 51 bytes, in .text
used by 5 · uses 2
  ADDRESS  INSTRUCTION
> 0x16c0   endbr64
  0x16c4   push    rbx
  0x16c5   cmp     byte ptr [rdi], 0
  0x16c8   mov     rbx, rdi
  0x16cb   je      0x16e8              jump ↓
  0x16cd   lea     rdi, [rdi+1]
  0x16d1   call    strlen@plt          call
  0x16d6   mov     rdi, rbx
  0x16d9   pop     rbx
  0x16da   lea     rsi, [rax+1]
  0x16de   jmp     platform_write@plt  tail call via PLT → platform_write
  0x16e3   nop     [rax+rax]
  0x16e8   mov     rdi, rbx
  0x16eb   xor     esi, esi
  0x16ed   pop     rbx
  0x16ee   jmp     platform_write@plt  tail call via PLT → platform_write









↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_cpp_function_names_what_it_calls() {
    assert_output(
        &screen("cpp-x86_64.elf", Some("process_frame"), (100, 30), &[]),
        "\
cpp-x86_64.elf › process_frame
app::process_frame(app::Frame const&, app::Stage&)  function at 0x15c0, 254 bytes, in .text
used by 1 · uses 7 · 1 call elfq can't follow
  ADDRESS  INSTRUCTION
> 0x15c0   endbr64
  0x15c4   sub     rsp, 0x58
  0x15c8   mov     rax, fs:[0x28]
  0x15d1   mov     [rsp+0x48], rax
  0x15d6   mov     rax, [rdi+8]
  0x15da   cmp     rax, 8
  0x15de   jbe     0x168a                      jump ↓
  0x15e4   mov     r8, [rdi]
  0x15e7   movzx   r9d, word ptr [r8+2]
  0x15ec   lea     rdx, [r9+9]
  0x15f0   cmp     rax, rdx
  0x15f3   jb      0x168a                      jump ↓
  0x15f9   mov     r11d, [r8+r9+5]
  0x15fe   mov     r10, rsi
  0x1601   lea     rdi, [r8+5]
  0x1605   mov     rsi, r9
  0x1608   call    crc32                       call
  0x160d   cmp     eax, r11d
  0x1610   jne     0x16a4                      jump ↓
  0x1616   cmp     r9, 0x3f
  0x161a   jbe     0x1680                      jump ↓
  0x161c   mov     r9d, 0x20
  0x1622   xor     eax, eax
  0x1624   mov     rdi, rsp

↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn thumb_code_shows_its_instructions_and_the_data_among_them() {
    // A word of a literal pool links to what it holds the address of.
    assert_output(
        &screen("c-cortex-m7.elf", Some("SysTick_Handler"), (80, 24), &[]),
        "\
c-cortex-m7.elf › SysTick_Handler
SysTick_Handler  function at 0x0, 16 bytes, in .itcm (0x00000000..0x000000e0),
copied there from 0x30000040
root: vector table · used by 1 · uses 2
  ADDRESS  INSTRUCTION
> 0x0      ldr     r3, [pc, #8]       uses systick_count in .bss
  0x2      ldr     r0, [r3]
  0x4      adds    r0, #1
  0x6      str     r0, [r3]
  0x8      b.w     __app_tick_veneer  tail call via veneer → app_tick in .text
  0xc      .word   0x20000444         systick_count in .bss











→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // A load of a word that holds no item's address shows the value, and
    // a jump to another line of the function says which way it goes.
    assert_output(
        &screen("c-cortex-m7.elf", Some("app_tick"), (100, 30), &[]),
        "\
c-cortex-m7.elf › app_tick
app_tick  function at 0x300002e4, 60 bytes, in .text (0x30000000..0x30000edc)
used by 2 · uses 3
  ADDRESS     INSTRUCTION
> 0x300002e4  ldr     r3, [pc, #44]   = 0xc28f5c29
  0x300002e6  ldr     r2, [pc, #48]   = 0x028f5c28
  0x300002e8  mul     r3, r0, r3
  0x300002ec  cmp.w   r2, r3, ror #2
  0x300002f0  push    {r4, lr}
  0x300002f2  mov     r4, r0
  0x300002f4  bhs     0x30000302      jump ↓
  0x300002f6  mov     r1, r4
  0x300002f8  movs    r0, #0
  0x300002fa  pop.w   {r4, lr}
  0x300002fe  b.w     dispatch_event  tail call
  0x30000302  ldr     r0, [pc, #24]   uses .rodata@0x30000de8 \"tick\\n\"
  0x30000304  bl      log_msg         call
  0x30000308  mov     r1, r4
  0x3000030a  movs    r0, #0
  0x3000030c  pop.w   {r4, lr}
  0x30000310  b.w     dispatch_event  tail call
  0x30000314  .word   0xc28f5c29
  0x30000318  .word   0x028f5c28
  0x3000031c  .word   0x30000de8      .rodata@0x30000de8





↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7.elf", Some("NMI_Handler"), (80, 24), &[]),
        "\
c-cortex-m7.elf › Default_Handler
Default_Handler (also named NMI_Handler)  function at 0x300005f4, 2 bytes, in
.text (0x30000000..0x30000edc)
root: vector table · used by 1 · uses 0
  ADDRESS     INSTRUCTION
> 0x300005f4  b       0x300005f4  jump ↑
















→ 0x300005f4, in this function
↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
}

#[test]
fn a_word_that_holds_an_items_address_links_to_it() {
    // Reset_Handler's literal pool: no load of these words links to stats
    // or config_sum, but the words that hold their addresses do. No item
    // starts at 0x08000dec, where the copy of .data starts in flash, or at
    // 0x20000048, where .bss ends.
    assert_output(
        &screen("c-cortex-m4.elf", Some("0x80005d8"), (100, 16), &[]),
        "\
c-cortex-m4.elf › Reset_Handler
Reset_Handler (which holds 0x80005d8)  function at 0x80005a0, 76 bytes, in .text
(0x08000000..0x08000e0c)
root: entry point · used by 1 · uses 3
  ADDRESS    INSTRUCTION
  0x80005cc  movs    r1, #0
  0x80005ce  bl      memset         call
  0x80005d2  bl      main           call
  0x80005d6  b       0x80005d6      jump ↑
> 0x80005d8  .word   0x20000000     stats in .data
  0x80005dc  .word   0x20000020     config_sum in .bss
  0x80005e0  .word   0x08000dec
  0x80005e4  .word   0x20000020     config_sum in .bss
  0x80005e8  .word   0x20000048
→ stats  data object at 0x20000000, 32 bytes, in .data
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_table_of_addresses_lists_the_cases_it_jumps_to() {
    // _strtod_l's switch on the first character: after cmp and bhi, adr
    // loads the address of the table, ldr.w pc jumps through it, and each
    // word of the table holds the Thumb address of a case. The stripped
    // file, which has no $d symbols, gives the same lines.
    assert_output(
        &screen("c-strtod.elf", Some("0x8000082"), (100, 16), &[]),
        "\
c-strtod.elf › _strtod_l
_strtod_l (which holds 0x8000082)  function at 0x8000058, 4084 bytes, in .text
(0x08000000..0x08005324)
used by 5 · uses 42 · 1 jump elfq can't follow
  ADDRESS    INSTRUCTION
  0x800007a  ldrb    r2, [r1], #1
  0x800007e  cmp     r2, #0x2d
  0x8000080  bhi     0x8000140             jump ↓
> 0x8000082  adr     r0, 0x8000088         address ↓
  0x8000084  ldr.w   pc, [r0, r2, lsl #2]  jump through a pointer: elfq can't follow
  0x8000088  .word   0x0800025b            jump ↓
  0x800008c  .word   0x08000141            jump ↓
  0x8000090  .word   0x08000141            jump ↓
  0x8000094  .word   0x08000141            jump ↓
→ 0x8000088, in this function
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-strtod-stripped.elf", Some("0x8000082"), (100, 16), &[]),
        "\
c-strtod-stripped.elf › fn_0x8000058
fn_0x8000058 (which holds 0x8000082)  function at 0x8000058, ~4124 bytes, in .text
(0x08000000..0x08005324)
found by following the code · used by 1 · uses 44 · 1 jump elfq can't follow
  ADDRESS    INSTRUCTION
  0x800007a  ldrb    r2, [r1], #1
  0x800007e  cmp     r2, #0x2d
  0x8000080  bhi     0x8000140             jump ↓
> 0x8000082  adr     r0, 0x8000088         address ↓
  0x8000084  ldr.w   pc, [r0, r2, lsl #2]  jump through a pointer: elfq can't follow
  0x8000088  .word   0x0800025b            jump ↓
  0x800008c  .word   0x08000141            jump ↓
  0x8000090  .word   0x08000141            jump ↓
  0x8000094  .word   0x08000141            jump ↓
→ 0x8000088, in this function
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // Enter on a word goes to its case, which here comes before the table,
    // and Esc comes back.
    let down = vec!["Down"; 11];
    let word = "> 0x80000ac  .word   0x08000077            jump ↑";
    assert_eq!(
        selected_and_status(&screen("c-strtod.elf", Some("0x8000082"), (100, 16), &down)),
        (word, "→ 0x8000076, in this function")
    );
    let case = screen(
        "c-strtod.elf",
        Some("0x8000082"),
        (100, 16),
        &[&down[..], &["Enter"]].concat(),
    );
    assert_eq!(
        selected_and_status(&case).0,
        "> 0x8000076  str     r1, [sp, #92]"
    );
    let back = [&down[..], &["Enter", "Esc"]].concat();
    let back = screen("c-strtod.elf", Some("0x8000082"), (100, 16), &back);
    assert_eq!(selected_and_status(&back).0, word);
}

#[test]
fn a_page_opened_at_an_address_shows_the_lines_before_it() {
    // The line comes a third of the way down, as after a jump inside the
    // function.
    assert_output(
        &screen("c-cortex-m7.elf", Some("0x3000034c"), (80, 24), &[]),
        "\
c-cortex-m7.elf › process_frame
process_frame (which holds 0x3000034c)  function at 0x30000320, 376 bytes, in
.text (0x30000000..0x30000edc)
used by 1 · uses 10
  ADDRESS     INSTRUCTION
  0x30000340  cmp     r3, r6
  0x30000342  bhi.w   0x3000045a             jump ↓
  0x30000346  adds    r7, r4, #5
  0x30000348  mov     r1, r5
  0x3000034a  mov     r0, r7
> 0x3000034c  bl      __crc32_veneer         call via veneer → crc32 in .itcm
  0x30000350  adds    r3, r4, r5
  0x30000352  ldr.w   r3, [r3, #5]
  0x30000356  cmp     r0, r3
  0x30000358  bne.w   0x30000470             jump ↓
  0x3000035c  cmp     r5, #0x41
  0x3000035e  bhi     0x3000044c             jump ↓
  0x30000360  lsrs    r5, r5, #1
  0x30000362  beq     0x30000450             jump ↓
  0x30000364  adds    r0, r4, #6
  0x30000366  add     r4, sp, #0x20
  0x30000368  movs    r3, #0
→ crc32  function at 0x10, 48 bytes, in .itcm, through __crc32_veneer
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_load_from_a_literal_pool_notes_what_it_reads() {
    // A word, as a float, and a double.
    let float = screen("c-vfp.elf", Some("0x8000062"), (100, 24), &[]);
    assert_eq!(
        selected_and_status(&float).0,
        "> 0x8000062  vldr    s15, [pc, #20]    = 0x00000000"
    );
    let double = screen("c-strtod.elf", Some("0x800025c"), (100, 16), &[]);
    assert_eq!(
        selected_and_status(&double).0,
        "> 0x800025c  vldr    d8, [pc, #496]        = 0x0000000000000000"
    );
}

#[test]
fn thumb_code_of_other_cores_lists_only_the_lines_that_use_another_item() {
    // The sample as if it were built for a Cortex-M33, whose instructions
    // elfq doesn't print.
    let data = damaged::for_arm_version("c-cortex-m7.elf", Some(17));
    let binary = Binary::parse(&data).unwrap();
    let page = |symbol| browse("c-cortex-m7.elf", &binary, Some(symbol), (80, 24), &[]).0;
    assert_output(
        &page("SysTick_Handler"),
        "\
c-cortex-m7.elf › SysTick_Handler
SysTick_Handler  function at 0x0, 16 bytes, in .itcm (0x00000000..0x000000e0),
copied there from 0x30000040
root: vector table · used by 1 · uses 2
  ADDRESS
> 0x0      uses systick_count in .bss
  0x8      tail call via veneer → app_tick in .text













elfq prints only the instructions of Cortex-M cores up to Armv7E-M, and this
file is for Armv8-M Mainline: these are the lines that use another item.
→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    // And a function that uses nothing says so.
    assert_output(
        &page("NMI_Handler"),
        "\
c-cortex-m7.elf › Default_Handler
Default_Handler (also named NMI_Handler)  function at 0x300005f4, 2 bytes, in
.text (0x30000000..0x30000edc)
root: vector table · used by 1 · uses 0
elfq prints only the instructions of Cortex-M cores up to Armv7E-M, and this
file is for Armv8-M Mainline. No line of Default_Handler uses another item.

















↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    // A file that doesn't say which core it is for.
    let data = damaged::for_arm_version("c-cortex-m7.elf", None);
    let binary = Binary::parse(&data).unwrap();
    assert_output(
        &browse(
            "c-cortex-m7.elf",
            &binary,
            Some("SysTick_Handler"),
            (80, 24),
            &[],
        )
        .0,
        "\
c-cortex-m7.elf › SysTick_Handler
SysTick_Handler  function at 0x0, 16 bytes, in .itcm (0x00000000..0x000000e0),
copied there from 0x30000040
root: vector table · used by 1 · uses 2
  ADDRESS
> 0x0      uses systick_count in .bss
  0x8      tail call via veneer → app_tick in .text













This file doesn't say which Arm core its code is for, so elfq doesn't print its
instructions: these are the lines that use another item.
→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_stripped_file_names_its_items_by_address() {
    assert_output(
        &screen("c-x86_64-stripped.elf", None, (80, 24), &[]),
        "\
c-x86_64-stripped.elf › fn_0x1170
fn_0x1170  function at 0x1170, 38 bytes, in .text
root: entry point · used by nothing · uses 2
  ADDRESS  INSTRUCTION
> 0x1170   endbr64
  0x1174   xor     ebp, ebp
  0x1176   mov     r9, rdx
  0x1179   pop     rsi
  0x117a   mov     rdx, rsp
  0x117d   and     rsp, 0xfffffffffffffff0
  0x1181   push    rax
  0x1182   push    rsp
  0x1183   xor     r8d, r8d
  0x1186   xor     ecx, ecx
  0x1188   lea     rdi, [rip-0xef]          address taken fn_0x10a0
  0x118f   call    qword ptr [rip+0x2e3b]   call __libc_start_main
  0x1195   hlt






↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7-stripped.elf", Some("0x0"), (100, 30), &[]),
        "\
c-cortex-m7-stripped.elf › fn_0x0
fn_0x0  function at 0x0, ~16 bytes, in .itcm (0x00000000..0x000000e0), copied there from 0x30000040
found by following the code · root: vector table · used by 1 · uses 2
  ADDRESS  INSTRUCTION
> 0x0      ldr     r3, [pc, #8]  uses .bss@0x20000444 in .bss
  0x2      ldr     r0, [r3]
  0x4      adds    r0, #1
  0x6      str     r0, [r3]
  0x8      b.w     fn_0xd0       tail call via veneer → fn_0x300002e4 in .text
  0xc      .word   0x20000444    .bss@0x20000444 in .bss


















→ .bss@0x20000444  data object at 0x20000444, ~4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn a_page_without_code_says_why() {
    assert_output(
        &screen("c-x86_64.elf", Some("_init"), (80, 24), &[]),
        "\
c-x86_64.elf › _init
_init  function at 0x1000, size unknown, in .init
root: in .init · used by nothing
elfq doesn't know where _init ends, so it can't read its code



















↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.elf", Some("frames_ok"), (80, 24), &[]),
        "\
c-x86_64.elf › frames_ok
frames_ok  data object at 0x4058, 4 bytes, in .bss
used by 2 · uses 0
frames_ok is a data object: it has no code



















↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
}

#[test]
fn a_small_screen_asks_for_more_room() {
    let small = screen("c-x86_64.elf", Some("log_msg"), (59, 16), &[]);
    assert_eq!(
        small.lines().next(),
        Some("make the terminal at least 60x16")
    );
    let small = screen("c-x86_64.elf", Some("log_msg"), (60, 15), &[]);
    assert_eq!(
        small.lines().next(),
        Some("make the terminal at least 60x16")
    );
    // On a narrower one, the message wraps, so that the size shows.
    let narrow = screen("c-x86_64.elf", Some("log_msg"), (26, 5), &[]);
    assert_eq!(narrow, "make the terminal at least\n60x16\n\n\n\n");
    // At the smallest size, the hint line leaves out what is needed least.
    assert_output(
        &screen("c-x86_64.elf", Some("main"), (60, 16), &[]),
        "\
c-x86_64.elf › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
  ADDRESS  INSTRUCTION
> 0x10a0   endbr64
  0x10a4   sub     rsp, 8
  0x10a8   lea     rsi, [rip+0x1b1]  address taken on_timer
  0x10af   xor     edi, edi
  0x10b1   call    register_handler  call
  0x10b6   lea     rsi, [rip+0x593]  address taken on_button
  0x10bd   mov     edi, 1
  0x10c2   call    register_handler  call
  0x10c7   lea     rsi, [rip+0x1a2]  address taken on_data
  0x10ce   mov     edi, 2

↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
}

#[test]
fn q_ctrl_c_and_ctrl_z_stop_the_event_loop() {
    let binary = load_sample("c-x86_64.elf");
    let stop = |script: &[&str]| browse(SAMPLE, &binary, None, (80, 24), script).1;
    assert_eq!(stop(&["q"]), Stop::Quit);
    assert_eq!(stop(&["Ctrl-C"]), Stop::Quit);
    #[cfg(unix)]
    assert_eq!(stop(&["Ctrl-Z"]), Stop::Suspend);
    // In tests, the keys run out.
    assert_eq!(stop(&["j", "x", "Ctrl-X"]), Stop::Quit);
}

#[test]
fn the_files_first_warning_is_on_the_first_page() {
    let data = damaged::past_the_end(&[".plt"]);
    let binary = Binary::parse(&data).unwrap();
    let (screen, _) = browse("plt-offset.elf", &binary, Some("main"), (80, 24), &[]);
    assert_eq!(
        selected_and_status(&screen).1,
        "warning: section .plt runs past the end of the file"
    );
    // Until the next key.
    let (screen, _) = browse("plt-offset.elf", &binary, Some("main"), (80, 24), &["j"]);
    assert_eq!(selected_and_status(&screen).1, "");
    // The others are counted.
    let data = damaged::past_the_end(&[".plt", ".plt.sec"]);
    let binary = Binary::parse(&data).unwrap();
    let (screen, _) = browse("plt-offset.elf", &binary, Some("main"), (100, 30), &[]);
    assert_eq!(
        selected_and_status(&screen).1,
        "warning: section .plt runs past the end of the file; and 1 more, printed when you quit"
    );
}

#[test]
fn a_long_chain_of_calls_can_be_followed() {
    // 10,000 functions that each call the one before, from the entry
    // point. The browser runs on the test's own thread, with its small
    // stack.
    let data = damaged::chain_of_calls(10_000);
    let binary = Binary::parse(&data).unwrap();
    let (screen, _) = browse(
        "long-chain.elf",
        &binary,
        None,
        (80, 24),
        &["Tab", "Enter", "Tab", "Enter", "Tab", "Enter"],
    );
    assert_output(
        &screen,
        "\
long-chain.elf › entry › fn_0x8113870 › fn_0x8113868 › fn_0x8113860
fn_0x8113860  function at 0x8113860, ~8 bytes, in .text (0x08100000..0x08113880)
found by following the code · used by 1 · uses 1
  ADDRESS    INSTRUCTION
> 0x8113860  push    {lr}
  0x8113862  bl      fn_0x8113858  call
  0x8113866  pop     {pc}
















↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}

#[test]
fn code_that_elfq_cant_read_is_refused() {
    // The x86-64 sample, marked as AArch64 code, as in the test of the
    // commands' message.
    let mut data = std::fs::read(sample_path("c-x86_64.elf")).unwrap();
    data[18..20].copy_from_slice(&183u16.to_le_bytes());
    let binary = Binary::parse(&data).unwrap();
    let error = Session::new(Path::new("aarch64.elf"), &binary)
        .err()
        .unwrap();
    assert_eq!(
        error.to_string(),
        "elfq can't yet read Aarch64 code to tell what calls what"
    );
}

#[test]
fn browse_needs_a_terminal() {
    // The tests' output isn't one, so the file isn't even read.
    for (args, error) in [
        (
            &[SAMPLE, "browse"][..],
            "error: browse needs a terminal; to print answers, run elfq testdata/bin/c-x86_64.elf \
             or another command\n",
        ),
        (
            &["my dir/app.elf", "browse", "main"],
            "error: browse needs a terminal; to print answers, run elfq 'my dir/app.elf' or \
             another command\n",
        ),
    ] {
        let output = elfq(args);
        assert_eq!(output.status.code(), Some(2));
        assert_output(&String::from_utf8(output.stderr).unwrap(), error);
    }
}

#[test]
fn used_by_counts_the_callers_that_the_callers_command_lists() {
    // `callers __rust_dealloc` says it "is used by 82 functions", in 118
    // rows: a function that calls it and also takes its address has a row
    // for each.
    let page = screen(
        "rust-x86_64-nodebug.elf",
        Some("__rust_dealloc"),
        (80, 24),
        &[],
    );
    let facts = page.lines().find(|line| line.starts_with("used by"));
    assert_eq!(facts, Some("used by 82 · uses 1"), "{page}");
}

#[test]
fn the_keys_move_as_the_help_says() {
    let selected = |sample, symbol, script: &[&str]| {
        let page = screen(sample, Some(symbol), (80, 24), script);
        selected_and_status(&page).0.to_string()
    };
    // Shift-Tab goes to the line before that uses another item, going
    // round the end.
    assert_eq!(
        selected("c-x86_64.elf", "log_msg", &["BackTab"]),
        "> 0x163e   jmp     platform_write     tail call"
    );
    // A page down shows the next page, with the cursor where it was on the
    // screen; Space does the same.
    for script in [&["PageDown"][..], &[" "]] {
        let page = screen("c-x86_64.elf", Some("main"), (80, 24), script);
        let lines: Vec<&str> = page.lines().collect();
        assert_eq!(lines[3], "  ADDRESS  INSTRUCTION", "{script:?}");
        assert!(lines[4].starts_with("> 0x10fc "), "{script:?}\n{page}");
    }
    for script in [&["PageDown", "PageUp"][..], &["End", "Home"]] {
        assert_eq!(
            selected("c-x86_64.elf", "main", script),
            "> 0x10a0   endbr64",
            "{script:?}"
        );
    }
    // Following another link after going back leaves nothing to go forward
    // to.
    let page = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (80, 24),
        &["Tab", "Tab", "Enter", "Esc", "BackTab", "Enter", "]"],
    );
    assert!(
        page.starts_with("c-x86_64.elf › log_msg › strlen@plt\n"),
        "{page}"
    );
}

#[test]
fn only_key_presses_count() {
    // Windows also reports each key's release.
    let binary = load_sample("c-x86_64.elf");
    let session = Session::new(Path::new(SAMPLE), &binary).unwrap();
    let mut app = App::new(session, Some("log_msg")).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let released = |code| {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    };
    let mut events = [released(KeyCode::Tab), released(KeyCode::Char('q'))].into_iter();
    let stop = event_loop(&mut terminal, &mut app, || Ok(events.next()), || Ok(false)).unwrap();
    // The keys ran out: q didn't quit.
    assert_eq!(stop, Stop::Quit);
    assert_eq!(events.next(), None);
    let row: String = (0..80)
        .map(|column| terminal.backend().buffer()[(column, 4)].symbol())
        .collect();
    assert!(row.starts_with("> 0x1610 "), "{row}");
}

/// The browser on `sample`, as [`browse`] gives it, but with each key
/// waiting before the browser takes it, as when the user pastes them or
/// types faster than the screen keeps up: the screen, and how many times
/// it was drawn.
fn burst(sample: &str, (width, height): (u16, u16), script: &[&str]) -> (String, usize) {
    let binary = load_sample(sample);
    let path = format!("testdata/bin/{sample}");
    let session = Session::new(Path::new(&path), &binary).unwrap();
    let mut app = App::new(session, None).unwrap();
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    let events = keys(script);
    let taken = Cell::new(0);
    let next_event = || {
        taken.set(taken.get() + 1);
        Ok(events.get(taken.get() - 1).cloned())
    };
    let more_waiting = || Ok(taken.get() < events.len());
    let stop = event_loop(&mut terminal, &mut app, next_event, more_waiting).unwrap();
    assert_eq!(stop, Stop::Quit);
    let buffer = terminal.backend().buffer();
    let mut drawn = String::new();
    for row in 0..height {
        let line: String = (0..width)
            .map(|column| buffer[(column, row)].symbol())
            .collect();
        drawn += line.trim_end();
        drawn.push('\n');
    }
    (drawn, terminal.get_frame().count())
}

#[test]
fn keys_already_waiting_are_taken_before_drawing() {
    // As when the user pastes into go to: what is typed waits for the
    // next draw, so the rows are listed once for it. The screen is drawn
    // before g, which opens the box, before Down, which moves by what the
    // screen shows, and at the end.
    let sample = "c-cortex-m7.elf";
    let script = ["g", "cr", "x", "Backspace", "c", "Down"];
    let (drawn, frames) = burst(sample, (100, 16), &script);
    assert_eq!(frames, 3);
    assert_output(&drawn, &screen(sample, None, (100, 16), &script));
    assert!(drawn.contains("\n> 0x10 "), "{drawn}");
}

#[test]
fn keys_that_wait_give_the_screen_that_each_key_in_turn_gives() {
    // A key that moves by what the screen shows, such as PageDown by a
    // screenful, waits until the screen shows the keys before it: after
    // text typed into go to, after Enter opens a page, on a help page, and
    // before the first screen.
    let scripts: &[(&str, &[&str])] = &[
        ("rust-x86_64-nodebug.elf", &["g", "e", "PageDown"]),
        (
            "rust-x86_64-nodebug.elf",
            &["g", "e", "PageDown", "PageDown", "PageUp"],
        ),
        (
            "rust-x86_64-nodebug.elf",
            &[
                "g", "e", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down",
                "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down",
                "Down", "Up", "Up", "Up",
            ],
        ),
        (
            "rust-x86_64-nodebug.elf",
            &["g", "main", "Enter", "PageDown"],
        ),
        ("rust-x86_64-nodebug.elf", &["PageDown"]),
        ("rust-x86_64-nodebug.elf", &["g", "e", "End"]),
        (
            "rust-x86_64-nodebug.elf",
            &["Tab", "Enter", "PageDown", "Up"],
        ),
        ("c-x86_64.elf", &["?", "End"]),
        ("c-x86_64.elf", &["?", "Down", "Down"]),
        ("c-x86_64.elf", &["?", "PageDown"]),
        ("c-x86_64.elf", &["g", "log_msg", "Enter"]),
        ("c-x86_64.elf", &["g", "mainz", "Backspace", "Enter"]),
        ("c-x86_64.elf", &["g", "zzz", "Esc", "PageDown"]),
        ("cpp-x86_64.elf", &["g", "app", "PageDown", "Enter", "End"]),
        (
            "c-cortex-m7.elf",
            &["g", "a", "Down", "x", "Backspace", "PageDown", "Enter"],
        ),
    ];
    for &(sample, script) in scripts {
        let (drawn, _) = burst(sample, (100, 16), script);
        assert_eq!(
            drawn,
            screen(sample, None, (100, 16), script),
            "{sample}: {script:?}"
        );
    }
}

#[test]
fn notes_say_where_in_an_item_a_line_uses_and_what_elfq_cant_follow() {
    let at = |sample, address| {
        let page = screen(sample, Some(address), (100, 30), &[]);
        let (selected, status) = selected_and_status(&page);
        (selected.to_string(), status.to_string())
    };
    assert_eq!(
        at("c-x86_64.elf", "0x17b7"),
        (
            "> 0x17b7   mov     edi, [rip+0x2867]           uses stats+0x4".to_string(),
            "→ stats  data object at 0x4020, 32 bytes, in .data".to_string()
        )
    );
    assert_eq!(
        at("cpp-x86_64.elf", "0x165a").0,
        "> 0x165a   call    qword ptr [rax]             call through a pointer: elfq can't follow"
    );
}

/// Runs elfq with `args` in a terminal of 100x30 that `script` makes, and
/// once it has drawn its hint line, types `keys`, if any. Returns its exit
/// status, 3 if it left the terminal's settings changed, and everything it
/// wrote, or `None` without `script`.
#[cfg(target_os = "linux")]
fn in_a_terminal(args: &[&str], keys: &str) -> Option<(Option<i32>, String)> {
    use std::io::{Read, Write};
    use std::process::Stdio;

    let quote = |arg: &str| format!("'{}'", arg.replace('\'', r"'\''"));
    let elfq = [env!("CARGO_BIN_EXE_elfq")].iter().chain(args);
    let elfq: Vec<String> = elfq.map(|arg| quote(arg)).collect();
    let shell = format!(
        "stty cols 100 rows 30; before=$(stty -g); {}; status=$?; \
         [ \"$(stty -g)\" = \"$before\" ] || status=3; exit $status",
        elfq.join(" ")
    );
    let child = std::process::Command::new("timeout")
        .args(["60", "script", "-qec", &shell, "/dev/null"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("{error}"),
    };
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut output = Vec::new();
    if !keys.is_empty() {
        // Keys typed before the screen is drawn can be lost.
        let mut buffer = [0; 4096];
        while !String::from_utf8_lossy(&output).contains("q quit") {
            let read = stdout.read(&mut buffer).unwrap();
            assert!(read > 0, "{}", String::from_utf8_lossy(&output));
            output.extend_from_slice(&buffer[..read]);
        }
        stdin.write_all(keys.as_bytes()).unwrap();
    }
    stdout.read_to_end(&mut output).unwrap();
    drop(stdin);
    let status = child.wait().unwrap();
    Some((status.code(), String::from_utf8_lossy(&output).into_owned()))
}

#[test]
#[cfg(target_os = "linux")]
fn the_files_warnings_are_printed_when_browse_quits_or_refuses_it() {
    let warning = "warning: section .plt runs past the end of the file";
    let path = damaged::save("plt-offset.elf", &damaged::past_the_end(&[".plt"]));
    let Some((status, output)) = in_a_terminal(&[&path, "browse", "log_msg"], "q") else {
        eprintln!("no `script` to run elfq in a terminal");
        return;
    };
    assert_eq!(status, Some(0), "{output:?}");
    // After the screen is back.
    let back = output.rfind("\x1b[?1049l").expect(&output);
    assert!(output[back..].contains(warning), "{output:?}");
    // And before the error, as the other commands print it, when elfq
    // can't read the file's code.
    let mut data = damaged::past_the_end(&[".plt"]);
    data[18..20].copy_from_slice(&183u16.to_le_bytes());
    let path = damaged::save("plt-offset-aarch64.elf", &data);
    let (status, output) = in_a_terminal(&[&path, "browse"], "").unwrap();
    assert_eq!(status, Some(2), "{output:?}");
    assert_eq!(
        output,
        format!("{warning}\r\nerror: elfq can't yet read Aarch64 code to tell what calls what\r\n")
    );
}

#[test]
fn go_to_says_why_it_lists_nothing() {
    // In the CLI's words.
    assert_output(
        &screen("cpp-x86_64.elf", None, (80, 24), &["g", "mainz"]),
        "\
cpp-x86_64.elf › main › go to
  ADDRESS  SIZE  KIND  SECTION  ITEM
no function or data object is named `mainz`; did you mean main?



















go to: mainz▏
↑↓ move  enter open  esc cancel
",
    );
    let import = screen("rust-x86_64-nodebug.elf", None, (80, 24), &["g", "memcpy"]);
    assert_eq!(
        import.lines().nth(2),
        Some("memcpy is imported from a shared library: its code isn't in this file"),
    );
}

#[test]
fn go_to_names_what_symbol_finds_as_the_commands_do() {
    // Another name of an item finds it, and its row and its page say so.
    assert_output(
        &screen("c-cortex-m7.elf", None, (100, 16), &["g", "NMI_Handler"]),
        "\
c-cortex-m7.elf › main › go to
  ADDRESS     SIZE  KIND      SECTION  ITEM
> 0x300005f4     2  function  .text    Default_Handler (also named NMI_Handler)











go to: NMI_Handler▏
↑↓ move  enter open  esc cancel
",
    );
    let page = screen(
        "c-cortex-m7.elf",
        None,
        (100, 16),
        &["g", "NMI_Handler", "Enter"],
    );
    assert_eq!(
        page.lines().nth(1),
        Some("Default_Handler (also named NMI_Handler)  function at 0x300005f4, 2 bytes, in .text"),
    );
}

#[test]
fn go_to_ignores_case_and_says_how_many_it_leaves_out() {
    assert_eq!(
        screen("c-cortex-m7.elf", None, (100, 16), &["g", "CRC"]),
        screen("c-cortex-m7.elf", None, (100, 16), &["g", "crc"])
            .replace("go to: crc", "go to: CRC")
    );
    // It lists 500 items, the largest first, and Home and End go to the
    // first and the last.
    assert_output(
        &screen(
            "rust-x86_64-nodebug.elf",
            None,
            (100, 16),
            &["g", "e", "End", "Home"],
        ),
        "\
rust-x86_64-nodebug.elf › main › go to
  ADDRESS   SIZE  KIND      SECTION             ITEM
> 0x18c00  15332  function  .text               <std::backtrace_rs::symbolize::gimli::Cache>::with_…
  0x336a3  12242  function  .text               <std::backtrace_rs::symbolize::gimli::Context>::new
  0x2f760   9301  function  .text               <gimli::read::dwarf::Unit<gimli::read::endian_slice…
  0x16b20   8244  function  .text               <core::cell::once::OnceCell<core::result::Result<ad…
  0x49d5d   6210  function  .text               miniz_oxide::inflate::core::decompress
  0x20b10   5196  function  .text               gimli::read::unit::parse_attribute::<gimli::read::e…
  0x2d960   4538  function  .text               <addr2line::function::Function<gimli::read::endian_…
  0x3c8c0   4161  function  .text               std::sys::pal::unix::stack_overflow::thread_info::s…
  0x37c50   3972  function  .text               <gimli::read::rnglists::RngListIter<gimli::read::en…
  0x15ba0   3964  function  .text               <core::cell::once::OnceCell<core::result::Result<ad…
  0x2b940   3222  function  .text               <std::backtrace_rs::symbolize::gimli::Mapping>::new…
and 389 more; type more of the name
go to: e▏
↑↓ move  enter open  esc cancel
",
    );
    let end = screen(
        "rust-x86_64-nodebug.elf",
        None,
        (100, 16),
        &["g", "e", "End"],
    );
    let (last, _) = selected_and_status(&end);
    assert_eq!(
        last,
        "> 0x4a40      25  data      .rodata             elfq_sample::FRAME_A"
    );
}

#[test]
fn the_message_over_go_to_wraps() {
    assert_output(
        &screen("cpp-x86_64.elf", Some("Gain"), (80, 24), &[]),
        "\
cpp-x86_64.elf › main › go to
no function or data object is named `Gain`; the compiler inlined its one copy
into main, which `inlined main` lists; did you mean 'vtable for Gain', 'typeinfo
for Gain', 'typeinfo name for Gain', Gain::run, main?
  ADDRESS  SIZE  KIND      SECTION       ITEM
> 0x12f0     56  function  .text         app::Gain::run(short*, unsigned long)
  0x3d38     24  data      .data.rel.ro  vtable for app::Gain
  0x3d78     24  data      .data.rel.ro  typeinfo for app::Gain
  0x2050     12  data      .rodata       typeinfo name for app::Gain













go to: Gain▏
↑↓ move  enter open  esc cancel
",
    );
}

#[test]
fn a_second_question_mark_opens_no_second_help() {
    let back = screen("c-x86_64.elf", None, (80, 24), &["?", "?", "Esc"]);
    assert!(back.starts_with("c-x86_64.elf › main\n"), "{back}");
}

#[test]
fn rust_and_shared_library_pages_at_the_other_size() {
    assert_output(
        &screen("rust-x86_64-nodebug.elf", Some("main"), (100, 30), &[]),
        "\
rust-x86_64-nodebug.elf › main
main  function at 0x15220, 46 bytes, in .text
used by 1 · uses 4
  ADDRESS  INSTRUCTION
> 0x15220  push    rax
  0x15221  mov     rcx, rsi
  0x15224  movzx   eax, byte ptr [rip-0xade3]  uses __rustc_debug_gdb_scripts_section__
  0x1522b  movsxd  rdx, edi
  0x1522e  lea     rax, [rip-0x695]            address taken elfq_sample::main
  0x15235  mov     [rsp], rax
  0x15239  lea     rsi, [rip+0x3dab8]          uses .data.rel.ro@0x52cf8
  0x15240  mov     rdi, rsp
  0x15243  xor     r8d, r8d
  0x15246  call    qword ptr [rip+0x3f924]     call lang_start_internal
  0x1524c  pop     rcx
  0x1524d  ret













↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.so", Some("app_tick"), (80, 24), &[]),
        "\
c-x86_64.so › app_tick
app_tick  function at 0x1770, 62 bytes, in .text
used by 1 · uses 3
  ADDRESS  INSTRUCTION
> 0x1770   endbr64
  0x1774   imul    eax, edi, 0xc28f5c29
  0x177a   push    rbx
  0x177b   mov     ebx, edi
  0x177d   ror     eax, 2
  0x1780   cmp     eax, 0x28f5c28
  0x1785   jbe     0x1798                jump ↓
  0x1787   mov     esi, ebx
  0x1789   xor     edi, edi
  0x178b   pop     rbx
  0x178c   jmp     dispatch_event@plt    tail call via PLT → dispatch_event
  0x1791   nop     [rax]
  0x1798   lea     rdi, [rip+0x869]      uses .rodata@0x2008 \"tick\\n\"
  0x179f   call    log_msg@plt           call via PLT → log_msg
  0x17a4   mov     esi, ebx
  0x17a6   xor     edi, edi
  0x17a8   pop     rbx
  0x17a9   jmp     dispatch_event@plt    tail call via PLT → dispatch_event

↑↓ move  enter follow  esc back  tab next link  g go to  ? help  q quit
",
    );
}
