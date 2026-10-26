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
    let mut app = App::new(session, symbol);
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
/// the command under the page, if there is one, and the hints.
fn selected_and_status(screen: &str) -> (&str, &str) {
    let lines: Vec<&str> = screen.lines().collect();
    let selected = lines
        .iter()
        .find(|line| line.starts_with("> "))
        .unwrap_or(&"");
    let command = lines.iter().position(|line| line.starts_with("$ elfq "));
    (selected, lines[command.unwrap_or(lines.len() - 1) - 1])
}

#[test]
fn a_function_shows_its_code_and_what_each_line_uses() {
    assert_output(
        &screen("c-x86_64.elf", Some("log_msg"), (100, 30), &[]),
        "\
c-x86_64.elf › log_msg
log_msg  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
[1 code]  2 callees   3 callers   4 reach
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








↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
> 0x1ac0   endbr64
  0x1ac4   mov     rdx, rsi
  0x1ac7   mov     rsi, rdi
  0x1aca   mov     edi, 1
  0x1acf   jmp     write@plt  tail call













↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
fn without_symbol_the_start_page_opens() {
    // What the overview says about the file, then where the program starts,
    // as `reach` lists it, under the blocks of memory in firmware.
    assert_output(
        &screen("c-cortex-m7.elf", None, (100, 24), &[]),
        "\
c-cortex-m7.elf
testdata/bin/c-cortex-m7.elf: 32-bit Arm executable, with symbols and DWARF debug info
29 functions and 34 data objects, 7835 bytes of code and data
  MEMORY                  SIZE  SECTIONS
  0x00000000..0x000000e0   224  .itcm
  0x20000000..0x20000450  1104  .data .bss
  0x20240000..0x20241000  4096  .ocram_bss
  0x30000000..0x30000edc  3804  .isr_vector .text .rodata .ARM.exidx, load images of .itcm .data
Where testdata/bin/c-cortex-m7.elf starts, and the bytes of code and data that each reaches in each
block of memory:
  0x00000000  0x20000000  0x20240000  0x30000000  WHY               START
>        200        1100        4096        2071  main function     main (may reach more)
         200        1100        4096        2345  Reset (entry po…  Reset_Handler (may reach more)
           0           0           0           2  NMI, HardFault …  Default_Handler
          24          24        4096         430  SysTick           SysTick_Handler (may reach more)




The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names
them.
→ main  function at 0x30000570, 132 bytes, in .text
$ elfq testdata/bin/c-cortex-m7.elf reach
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.elf", None, (80, 24), &[]),
        "\
c-x86_64.elf
testdata/bin/c-x86_64.elf: x86-64 position-independent executable, with symbols
and DWARF debug info
32 functions and 36 data objects, 4021 bytes of code and data, plus 8 imports
Where testdata/bin/c-x86_64.elf starts, and the bytes of code and data that each
reaches:
  CODE  DATA  WHY                    START
> 2218  1379  main function          main (may reach more)
  2256  1379  entry point            _start (may reach more)
    80     1  listed in .init_array  frame_dummy (may reach more)
   128    25  listed in .fini_array  __do_global_dtors_aux (may reach more)






The starts marked (may reach more) reach calls or jumps that elfq can't follow;
reach START names them.
_init and _fini are starts too, but elfq doesn't know where they end, so it
can't count what they reach.
→ main  function at 0x10a0, 195 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf reach
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // A shared library's main isn't a start, and the CLI's note says that
    // its exported functions aren't either.
    assert_output(
        &screen("c-x86_64.so", None, (100, 24), &[]),
        "\
c-x86_64.so
testdata/bin/c-x86_64.so: x86-64 shared library, with symbols and DWARF debug info
38 functions and 34 data objects, 4197 bytes of code and data, plus 7 imports
Where testdata/bin/c-x86_64.so starts, and the bytes of code and data that each reaches:
  CODE  DATA  WHY                    START
>   80     1  listed in .init_array  frame_dummy (may reach more)
   128    33  listed in .fini_array  __do_global_dtors_aux (may reach more)









The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names
them.
_init and _fini are starts too, but elfq doesn't know where they end, so it can't count what they
reach.
elfq doesn't count a shared library's exported functions as starting points yet.
→ frame_dummy  function at 0x1300, ~16 bytes, in .text
$ elfq testdata/bin/c-x86_64.so reach
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // A stripped file names its starts by address, but for its entry
    // point.
    assert_output(
        &screen("c-cortex-m7-stripped.elf", None, (100, 24), &[]),
        "\
c-cortex-m7-stripped.elf
testdata/bin/c-cortex-m7-stripped.elf: 32-bit Arm executable, stripped
29 functions and 30 data objects, 7844 bytes of code and data
Items without symbols are named by address, like fn_0x0 and .data@0x20000000.
  MEMORY                  SIZE  SECTIONS
  0x00000000..0x000000e0   224  .itcm
  0x20000000..0x20000450  1104  .data .bss
  0x20240000..0x20241000  4096  .ocram_bss
  0x30000000..0x30000edc  3804  .isr_vector .text .rodata .ARM.exidx, load images of .itcm .data
Where testdata/bin/c-cortex-m7-stripped.elf starts, and the bytes of code and data that each reaches
in each block of memory:
  0x00000000  0x20000000  0x20240000  0x30000000  WHY                        START
>        200        1100        4096        2354  Reset (entry point)        entry (may reach more)
           0           0           0           2  NMI, HardFault and 6 more  fn_0x300005f4
          24          24        4096         430  SysTick                    fn_0x0 (may reach more)




The starts marked (may reach more) reach calls or jumps that elfq can't follow; reach START names
them.
→ entry  function at 0x300005f8, ~112 bytes, in .text
$ elfq testdata/bin/c-cortex-m7-stripped.elf reach
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}

#[test]
fn enter_opens_a_start_and_esc_comes_back_to_the_start_page() {
    let script = ["Down", "Down", "Down", "Enter"];
    assert_output(
        &screen("c-cortex-m7.elf", None, (100, 24), &script),
        "\
c-cortex-m7.elf › SysTick_Handler
SysTick_Handler  function at 0x0, 16 bytes, in .itcm (0x00000000..0x000000e0), copied there from
0x30000040
root: vector table · used by 1 · uses 2
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
> 0x0      ldr     r3, [pc, #8]       uses systick_count in .bss
  0x2      ldr     r0, [r3]
  0x4      adds    r0, #1
  0x6      str     r0, [r3]
  0x8      b.w     __app_tick_veneer  tail call via veneer → app_tick in .text
  0xc      .word   0x20000444         systick_count in .bss










→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_eq!(
        screen(
            "c-cortex-m7.elf",
            None,
            (100, 24),
            &[&script[..], &["Esc"]].concat()
        ),
        screen("c-cortex-m7.elf", None, (100, 24), &script[..3])
    );
    // It is the first page.
    let first = screen("c-cortex-m7.elf", None, (100, 24), &["Esc"]);
    let (_, status) = selected_and_status(&first);
    assert_eq!(status, "this is the first page; q quits");
}

#[test]
fn the_start_page_on_a_small_screen_keeps_its_selected_row() {
    assert_output(
        &screen("c-cortex-m7.elf", None, (60, 16), &["Down"]),
        "\
c-cortex-m7.elf
testdata/bin/c-cortex-m7.elf: 32-bit Arm executable, with
symbols and DWARF debug info
29 functions and 34 data objects, 7835 bytes of code and
data …
Where testdata/bin/c-cortex-m7.elf starts, and the bytes of
code and data that each reaches in each block of memory:
  0x00000000  0x20000000  0x20240000  0x30000000  …  START
         200        1100        4096        2071  …  main (…
>        200        1100        4096        2345  …  Reset_…
           0           0           0           2  …  Defaul…
The starts marked (may reach more) reach calls or jumps that
elfq can't follow; reach START names them.
→ Reset_Handler  function at 0x300005f8, 112 bytes, in .text
$ elfq testdata/bin/c-cortex-m7.elf reach
↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.elf", None, (60, 16), &[]),
        "\
c-x86_64.elf
testdata/bin/c-x86_64.elf: x86-64 position-independent
executable, with symbols and DWARF debug info …
Where testdata/bin/c-x86_64.elf starts, and the bytes of
code and data that each reaches:
  CODE  DATA  WHY           START
> 2218  1379  main functi…  main (may reach more)
  2256  1379  entry point   _start (may reach more)
    80     1  listed in .…  frame_dummy (may reach more)
The starts marked (may reach more) reach calls or jumps that
elfq can't follow; reach START names them.
_init and _fini are starts too, but elfq doesn't know where
they end, so it can't count what they reach.
→ main  function at 0x10a0, 195 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf reach
↑↓ move  enter follow  esc back  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // An import without a PLT stub has a page of its own, with what calls
    // it.
    assert_output(
        &screen("rust-x86_64-nodebug.elf", Some("memcpy"), (80, 24), &[]),
        "\
rust-x86_64-nodebug.elf › memcpy
memcpy  imported from a shared library: its code isn't in this file
used by 57
[3 callers]
  HOW            AT                  CALLER
> call           0x4cced             <&[u8] as SpecNewImpl>::spec_new_impl
  call           0x2ebed             <LoopingLookup<…>>::new_lookup
  call           0x2d0a6 and 2 more  <BalancingContext<…>>::bulk_steal_left
  call           0x2d458 and 2 more  <BalancingContext<…>>::bulk_steal_right
  address taken  0x1c8e6             <BalancingContext<…>>::merge_tracking_chil…
  call           0x1cad9             <BalancingContext<…>>::merge_tracking_chil…
  call           0x430f8             …Abbreviation, Internal>, Edge>>::insert::…
  call           0x434ad             <Handle<NodeRef<…>, KV>>::split::<Global>
  call           0x43f3b, 0x43f53    <Handle<NodeRef<…>, KV>>::split_leaf_data
  call           0x42fbb             …Abbreviation, Leaf>, Edge>>::insert::<Glo…
  address taken  0x1d1e1             <Handle<…>>::remove_leaf_kv::<…>
  call           0x1d3bf             <Handle<…>>::remove_leaf_kv::<…>
  call           0x4cc56             <String as From<Cow<str>>>::from
  call           0x3f93f             <String as Write>::write_str
  call           0x4c87a, 0x4c900    <alloc::string::String>::from_utf8_lossy
  call           0x3f6db             <Vec<u8> as Write>::write
→ <&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl  fun…
$ elfq testdata/bin/rust-x86_64-nodebug.elf callers memcpy --all
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("rust-x86_64-nodebug.elf", Some("memcpy"), (100, 30), &[]),
        "\
rust-x86_64-nodebug.elf › memcpy
memcpy  imported from a shared library: its code isn't in this file
used by 57
[3 callers]
  HOW            AT                  CALLER
> call           0x4cced             <&[u8] as SpecNewImpl>::spec_new_impl
  call           0x2ebed             <LoopingLookup<…>>::new_lookup
  call           0x2d0a6 and 2 more  <BalancingContext<usize, ThreadInfo>>::bulk_steal_left
  call           0x2d458 and 2 more  <BalancingContext<usize, ThreadInfo>>::bulk_steal_right
  address taken  0x1c8e6             <BalancingContext<…>>::merge_tracking_child_edge::<Global>
  call           0x1cad9             <BalancingContext<…>>::merge_tracking_child_edge::<Global>
  call           0x430f8             …ef<Mut, u64, Abbreviation, Internal>, Edge>>::insert::<Global>
  call           0x434ad             <Handle<NodeRef<…>, KV>>::split::<Global>
  call           0x43f3b, 0x43f53    <Handle<NodeRef<…>, KV>>::split_leaf_data
  call           0x42fbb             …odeRef<Mut, u64, Abbreviation, Leaf>, Edge>>::insert::<Global>
  address taken  0x1d1e1             <Handle<…>>::remove_leaf_kv::<remove_kv::{closure#0}, Global>
  call           0x1d3bf             <Handle<…>>::remove_leaf_kv::<remove_kv::{closure#0}, Global>
  call           0x4cc56             <String as From<Cow<str>>>::from
  call           0x3f93f             <alloc::string::String as core::fmt::Write>::write_str
  call           0x4c87a, 0x4c900    <alloc::string::String>::from_utf8_lossy
  call           0x3f6db             <alloc::vec::Vec<u8> as std::io::Write>::write
  call           0x3f74f             <alloc::vec::Vec<u8> as std::io::Write>::write_all
  call           0x3f65a             <alloc::vec::Vec<u8> as std::io::Write>::write_all_vectored
  call           0x3f4d1             <alloc::vec::Vec<u8> as std::io::Write>::write_vectored
  call           0x16bda, 0x16d5a    <OnceCell<…>>::try_init::<get_or_init<…>::{closure#0}, !>
  call           0x30b2c             <Unit<EndianSlice<LittleEndian>, usize>>::new
  call           0x195cb and 8 more  <Cache>::with_global::<resolve::{closure#1}>
→ <&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl  function at 0x4cc90, 30…
$ elfq testdata/bin/rust-x86_64-nodebug.elf callers memcpy --all
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}

#[test]
fn a_symbol_that_finds_several_items_or_none_opens_go_to() {
    // On the start page, which opens without SYMBOL, with the CLI's
    // message over the rows.
    assert_output(
        &screen("cpp-x86_64.elf", Some("log"), (80, 24), &[]),
        "\
cpp-x86_64.elf › go to
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
    assert!(
        page.starts_with("c-x86_64.elf\ntestdata/bin/c-x86_64.elf: "),
        "{page}"
    );
}

#[test]
fn g_goes_to_what_is_typed() {
    // Names that have the text, largest first, after what SYMBOL finds.
    assert_output(
        &screen("c-cortex-m7.elf", Some("main"), (100, 16), &["g", "crc"]),
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
        Some("main"),
        (100, 16),
        &["g", "crc", "Down", "Enter"],
    );
    assert!(
        opened.starts_with("c-cortex-m7.elf › main › crc32\n"),
        "{opened}"
    );
    // An address opens the item that holds it, at that line.
    assert_output(
        &screen(
            "c-x86_64.elf",
            Some("main"),
            (80, 24),
            &["g", "0x162e", "Enter"],
        ),
        "\
c-x86_64.elf › main › log_msg
log_msg (which holds 0x162e)  function at 0x1610, 51 bytes, in .text
used by 5 · uses 2
[1 code]  2 callees   3 callers   4 reach
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
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // Keys type, but for Ctrl-C and Ctrl-Z, and Esc cancels.
    let typed = screen(
        "c-x86_64.elf",
        Some("main"),
        (80, 24),
        &["g", "q?j", "Ctrl-O"],
    );
    assert_eq!(typed.lines().nth(22), Some("go to: q?j▏"));
    let cancelled = screen("c-x86_64.elf", Some("main"), (80, 24), &["g", "log", "Esc"]);
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
    let empty = screen("c-x86_64.elf", Some("main"), (80, 24), &["g"]);
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
  Enter → l            follow the line's link, or open the row
  Esc ← h Bksp Ctrl-O  back to where you were
  ]                    forward again
  Tab Shift-Tab        the next or previous line that uses another item
  1 2 3 4              tabs: 1 code, 2 callees, 3 callers, 4 reach
  /                    find text in the page's rows, ignoring case
  n N                  the next or previous row that has the text found
  g                    go to a function, data object or import, by name or address
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

↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    // On smaller screens, the lines wrap where they must, and the rest
    // is a scroll away.
    assert_output(
        &screen("c-cortex-m7.elf", Some("app_tick"), (100, 30), &["?"]),
        "\
c-cortex-m7.elf › app_tick › help
Keys
  ↑ ↓  k j             move
  PgUp PgDn Space      a page up or down
  Home End             the first or last line
  Enter → l            follow the line's link, or open the row
  Esc ← h Bksp Ctrl-O  back to where you were
  ]                    forward again
  Tab Shift-Tab        the next or previous line that uses another item
  1 2 3 4              tabs: 1 code, 2 callees, 3 callers, 4 reach
  /                    find text in the page's rows, ignoring case
  n N                  the next or previous row that has the text found
  g                    go to a function, data object or import, by name or address
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

↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7.elf", Some("app_tick"), (80, 24), &["?"]),
        "\
c-cortex-m7.elf › app_tick › help
Keys
  ↑ ↓  k j             move
  PgUp PgDn Space      a page up or down
  Home End             the first or last line
  Enter → l            follow the line's link, or open the row
  Esc ← h Bksp Ctrl-O  back to where you were
  ]                    forward again
  Tab Shift-Tab        the next or previous line that uses another item
  1 2 3 4              tabs: 1 code, 2 callees, 3 callers, 4 reach
  /                    find text in the page's rows, ignoring case
  n N                  the next or previous row that has the text found
  g                    go to a function, data object or import, by name or
                       address
  ?                    this help
  Ctrl-Z               back to the shell; fg returns
  q Ctrl-C             quit
Words
  call                 a direct call, or a call through a register that the code
                       loaded with the target's address, as a long call does
  tail call            a jump to the start of a function, which then returns to
                       the caller's caller

↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
    // It scrolls, and Esc goes back.
    let end = screen("c-x86_64.elf", Some("main"), (60, 16), &["?", "End"]);
    assert!(
        end.contains("\n  ~12  ?               an estimated size, and an unknown\n"),
        "{end}"
    );
    assert!(
        end.contains("\n  address ↓  address ↑ adr loads the address of a line in\n"),
        "{end}"
    );
    let back = screen("c-x86_64.elf", Some("main"), (80, 24), &["?", "Esc"]);
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
[1 code]  2 callees   3 callers   4 reach
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






↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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








↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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

↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
> 0x0      ldr     r3, [pc, #8]       uses systick_count in .bss
  0x2      ldr     r0, [r3]
  0x4      adds    r0, #1
  0x6      str     r0, [r3]
  0x8      b.w     __app_tick_veneer  tail call via veneer → app_tick in .text
  0xc      .word   0x20000444         systick_count in .bss










→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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




↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7.elf", Some("NMI_Handler"), (80, 24), &[]),
        "\
c-cortex-m7.elf › Default_Handler
Default_Handler (also named NMI_Handler)  function at 0x300005f4, 2 bytes, in
.text (0x30000000..0x30000edc)
root: vector table · used by 1 · uses 0
[1 code]  2 callees   3 callers   4 reach
  ADDRESS     INSTRUCTION
> 0x300005f4  b       0x300005f4  jump ↑















→ 0x300005f4, in this function
↑↓ move  enter follow  esc back  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS    INSTRUCTION
  0x80005ce  bl      memset         call
  0x80005d2  bl      main           call
  0x80005d6  b       0x80005d6      jump ↑
> 0x80005d8  .word   0x20000000     stats in .data
  0x80005dc  .word   0x20000020     config_sum in .bss
  0x80005e0  .word   0x08000dec
  0x80005e4  .word   0x20000020     config_sum in .bss
  0x80005e8  .word   0x20000048
→ stats  data object at 0x20000000, 32 bytes, in .data
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS    INSTRUCTION
  0x800007e  cmp     r2, #0x2d
  0x8000080  bhi     0x8000140             jump ↓
> 0x8000082  adr     r0, 0x8000088         address ↓
  0x8000084  ldr.w   pc, [r0, r2, lsl #2]  jump through a pointer: elfq can't follow
  0x8000088  .word   0x0800025b            jump ↓
  0x800008c  .word   0x08000141            jump ↓
  0x8000090  .word   0x08000141            jump ↓
  0x8000094  .word   0x08000141            jump ↓
→ 0x8000088, in this function
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-strtod-stripped.elf", Some("0x8000082"), (100, 16), &[]),
        "\
c-strtod-stripped.elf › fn_0x8000058
fn_0x8000058 (which holds 0x8000082)  function at 0x8000058, ~4124 bytes, in .text
(0x08000000..0x08005324)
found by following the code · used by 1 · uses 44 · 1 jump elfq can't follow
[1 code]  2 callees   3 callers   4 reach
  ADDRESS    INSTRUCTION
  0x800007e  cmp     r2, #0x2d
  0x8000080  bhi     0x8000140             jump ↓
> 0x8000082  adr     r0, 0x8000088         address ↓
  0x8000084  ldr.w   pc, [r0, r2, lsl #2]  jump through a pointer: elfq can't follow
  0x8000088  .word   0x0800025b            jump ↓
  0x800008c  .word   0x08000141            jump ↓
  0x8000090  .word   0x08000141            jump ↓
  0x8000094  .word   0x08000141            jump ↓
→ 0x8000088, in this function
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
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
→ crc32  function at 0x10, 48 bytes, in .itcm, through __crc32_veneer
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS
> 0x0      uses systick_count in .bss
  0x8      tail call via veneer → app_tick in .text












elfq prints only the instructions of Cortex-M cores up to Armv7E-M, and this
file is for Armv8-M Mainline: these are the lines that use another item.
→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
elfq prints only the instructions of Cortex-M cores up to Armv7E-M, and this
file is for Armv8-M Mainline. No line of Default_Handler uses another item.
















↑↓ move  enter follow  esc back  1 2 3 4 tabs  g go to  ? help  q quit
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
[1 code]  2 callees   3 callers   4 reach
  ADDRESS
> 0x0      uses systick_count in .bss
  0x8      tail call via veneer → app_tick in .text












This file doesn't say which Arm core its code is for, so elfq doesn't print its
instructions: these are the lines that use another item.
→ systick_count  data object at 0x20000444, 4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}

#[test]
fn a_stripped_file_names_its_items_by_address() {
    assert_output(
        &screen("c-x86_64-stripped.elf", Some("fn_0x1170"), (80, 24), &[]),
        "\
c-x86_64-stripped.elf › fn_0x1170
fn_0x1170  function at 0x1170, 38 bytes, in .text
root: entry point · used by nothing · uses 2
[1 code]  2 callees   3 callers   4 reach
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





↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7-stripped.elf", Some("0x0"), (100, 30), &[]),
        "\
c-cortex-m7-stripped.elf › fn_0x0
fn_0x0  function at 0x0, ~16 bytes, in .itcm (0x00000000..0x000000e0), copied there from 0x30000040
found by following the code · root: vector table · used by 1 · uses 2
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
> 0x0      ldr     r3, [pc, #8]  uses .bss@0x20000444 in .bss
  0x2      ldr     r0, [r3]
  0x4      adds    r0, #1
  0x6      str     r0, [r3]
  0x8      b.w     fn_0xd0       tail call via veneer → fn_0x300002e4 in .text
  0xc      .word   0x20000444    .bss@0x20000444 in .bss

















→ .bss@0x20000444  data object at 0x20000444, ~4 bytes, in .bss
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
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
[1 code]  3 callers
elfq doesn't know where _init ends, so it can't read its code


















↑↓ move  enter follow  esc back  1 3 tabs  g go to  ? help  q quit
",
    );
    // A data object's page opens on its callers.
    assert_output(
        &screen("c-x86_64.elf", Some("frames_ok"), (80, 24), &[]),
        "\
c-x86_64.elf › frames_ok
frames_ok  data object at 0x4058, 4 bytes, in .bss
used by 2 · uses 0
 2 callees  [3 callers]  4 reach
  HOW   AT      CALLER
> uses  0x1892  process_frame
  uses  0x1961  report














→ process_frame  function at 0x1700, 532 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf callers frames_ok
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // A data object has no code tab, and 1 says why.
    let page = screen("c-x86_64.elf", Some("frames_ok"), (80, 24), &["1"]);
    let (_, status) = selected_and_status(&page);
    assert_eq!(status, "frames_ok is a data object: it has no code");
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
[1 code]  2 callees   3 callers   4 reach
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
    // point, which the start page lists. The browser runs on the test's
    // own thread, with its small stack.
    let data = damaged::chain_of_calls(10_000);
    let binary = Binary::parse(&data).unwrap();
    let (screen, _) = browse(
        "long-chain.elf",
        &binary,
        None,
        (80, 24),
        &["Enter", "Tab", "Enter", "Tab", "Enter", "Tab", "Enter"],
    );
    assert_output(
        &screen,
        "\
long-chain.elf › entry › fn_0x8113870 › fn_0x8113868 › fn_0x8113860
fn_0x8113860  function at 0x8113860, ~8 bytes, in .text (0x08100000..0x08113880)
found by following the code · used by 1 · uses 1
[1 code]  2 callees   3 callers   4 reach
  ADDRESS    INSTRUCTION
> 0x8113860  push    {lr}
  0x8113862  bl      fn_0x8113858  call
  0x8113866  pop     {pc}















↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
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
fn three_lists_the_callers_that_the_callers_command_lists() {
    assert_output(
        &screen("c-cortex-m7.elf", Some("log_msg"), (100, 30), &["3"]),
        "\
c-cortex-m7.elf › log_msg
log_msg  function at 0x30000288, 26 bytes, in .text (0x30000000..0x30000edc)
used by 4 · uses 2
 1 code   2 callees  [3 callers]  4 reach
  HOW        AT                      CALLER
> call       0x30000304              app_tick
  tail call  0x300002b0              on_button
  call       0x30000464, 0x3000047a  process_frame
  call       0x3000053c              report


















→ app_tick  function at 0x300002e4, 60 bytes, in .text
$ elfq testdata/bin/c-cortex-m7.elf callers log_msg
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-cortex-m7.elf", Some("log_msg"), (80, 24), &["3"]),
        "\
c-cortex-m7.elf › log_msg
log_msg  function at 0x30000288, 26 bytes, in .text (0x30000000..0x30000edc)
used by 4 · uses 2
 1 code   2 callees  [3 callers]  4 reach
  HOW        AT                      CALLER
> call       0x30000304              app_tick
  tail call  0x300002b0              on_button
  call       0x30000464, 0x3000047a  process_frame
  call       0x3000053c              report












→ app_tick  function at 0x300002e4, 60 bytes, in .text
$ elfq testdata/bin/c-cortex-m7.elf callers log_msg
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // A shared library calls its own functions through PLT stubs, and
    // says so under the rows, as `callers` does.
    assert_output(
        &screen("c-x86_64.so", Some("log_msg"), (80, 24), &["3"]),
        "\
c-x86_64.so › log_msg
log_msg  function at 0x16c0, 51 bytes, in .text
used by 5 · uses 2
 1 code   2 callees  [3 callers]  4 reach
  HOW        AT              CALLER
> call       0x179f          app_tick
  call       0x1b5f          debug_dump
  tail call  0x1711          on_button
  call       0x199e, 0x19b8  process_frame
  call       0x1b22          report










This includes calls through log_msg@plt.
→ app_tick  function at 0x1770, 62 bytes, in .text
$ elfq testdata/bin/c-x86_64.so callers log_msg
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // 1 shows the code again, as it was left.
    assert_eq!(
        screen("c-x86_64.elf", Some("0x162e"), (80, 24), &["3", "1"]),
        screen("c-x86_64.elf", Some("0x162e"), (80, 24), &[])
    );
}

#[test]
fn long_names_are_shortened_where_they_are_listed() {
    // Inside brackets, paths keep their last part, and brackets become
    // <…> as the room needs, but the two `split` rows, which differ only
    // in Internal and Leaf, show where they differ. The status line has
    // the selected name in full, and the command, SYMBOL as the CLI's
    // answer names it.
    assert_output(
        &screen(
            "rust-x86_64-nodebug.elf",
            Some("__rust_dealloc"),
            (80, 24),
            &["3"],
        ),
        "\
rust-x86_64-nodebug.elf › __rust_dealloc
__rustc::__rust_dealloc  function at 0x15260, 5 bytes, in .text
used by 82 · uses 1
 1 code   2 callees  [3 callers]  4 reach
  HOW            AT                   CALLER
> call           0x4cdb1              <&[u8] as SpecNewImpl>::spec_new_impl
  call           0x3e877, 0x3e8d5     <DisplayBacktrace as Display>::fmt
  call           0x2f079              <LoopingLookup<…>>::new_lookup
  address taken  0x39925, 0x39a37     <IntoIter<u64, Result<…>>>::dying_next
  call           0x39973, 0x39bd9     <IntoIter<u64, Result<…>>>::dying_next
  address taken  0x39ce5, 0x39df7     <IntoIter<u64, Abbreviation>>::dying_next
  call           0x39d33, 0x39f99     <IntoIter<u64, Abbreviation>>::dying_next
  call           0x1cb93              <BalancingContext<…>>::merge_tracking_chi…
  call           0x4358d              …Abbreviation, Internal>, KV>>::split::<G…
  call           0x43401              …Abbreviation, Leaf>, KV>>::split::<Globa…
  call           0x1d018, 0x1d558     <Handle<…>>::remove_leaf_kv::<…>
  call           0x42e9e              <NodeRef<…>>::new_internal::<Global>
  call           0x4cb03, 0x4cb80     <CString>::_from_vec_unchecked
  tail call      0x43652              <alloc::raw_vec::RawVecInner>::deallocate
Calls to __rustc::__rust_dealloc through a pointer aren't listed, since elfq
can't see them.
→ <&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl  fun…
$ elfq testdata/bin/rust-x86_64-nodebug.elf callers __rust_dealloc --all
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen(
            "rust-x86_64-nodebug.elf",
            Some("__rust_dealloc"),
            (100, 30),
            &["3"],
        ),
        "\
rust-x86_64-nodebug.elf › __rust_dealloc
__rustc::__rust_dealloc  function at 0x15260, 5 bytes, in .text
used by 82 · uses 1
 1 code   2 callees  [3 callers]  4 reach
  HOW            AT                   CALLER
> call           0x4cdb1              <&[u8] as SpecNewImpl>::spec_new_impl
  call           0x3e877, 0x3e8d5     <DisplayBacktrace as Display>::fmt
  call           0x2f079              <LoopingLookup<…>>::new_lookup
  address taken  0x39925, 0x39a37     <IntoIter<u64, Result<Arc<Abbreviations>, Error>>>::dying_next
  call           0x39973, 0x39bd9     <IntoIter<u64, Result<Arc<Abbreviations>, Error>>>::dying_next
  address taken  0x39ce5, 0x39df7     <IntoIter<u64, Abbreviation>>::dying_next
  call           0x39d33, 0x39f99     <IntoIter<u64, Abbreviation>>::dying_next
  call           0x1cb93              <BalancingContext<…>>::merge_tracking_child_edge::<Global>
  call           0x4358d              …eRef<Mut, u64, Abbreviation, Internal>, KV>>::split::<Global>
  call           0x43401              …<NodeRef<Mut, u64, Abbreviation, Leaf>, KV>>::split::<Global>
  call           0x1d018, 0x1d558     <Handle<…>>::remove_leaf_kv::<remove_kv::{closure#0}, Global>
  call           0x42e9e              <NodeRef<…>>::new_internal::<Global>
  call           0x4cb03, 0x4cb80     <alloc::ffi::c_str::CString>::_from_vec_unchecked
  tail call      0x43652              <alloc::raw_vec::RawVecInner>::deallocate
  call           0x4c9ed              <alloc::string::String>::from_utf8_lossy
  tail call      0x39461              <Arc<Abbreviations>>::drop_slow
  call           0x39489              <Arc<Abbreviations>>::drop_slow
  tail call      0x39393              <Arc<Dwarf<EndianSlice<LittleEndian>>>>::drop_slow
  call           0x393d2              <Arc<Dwarf<EndianSlice<LittleEndian>>>>::drop_slow
  call           0x393fb              <Arc<Mutex<Vec<u8>>>>::drop_slow
  tail call      0x39421              <Arc<Mutex<Vec<u8>>>>::drop_slow
Calls to __rustc::__rust_dealloc through a pointer aren't listed, since elfq can't see them.
→ <&[u8] as <alloc::ffi::c_str::CString>::new::SpecNewImpl>::spec_new_impl  function at 0x4cc90, 30…
$ elfq testdata/bin/rust-x86_64-nodebug.elf callers __rust_dealloc --all
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    // A page's name in the title is shortened to fit after the file's,
    // and the trail is cut from the left.
    let script = [
        "3", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Enter",
    ];
    let page = screen(
        "rust-x86_64-nodebug.elf",
        Some("__rust_dealloc"),
        (80, 24),
        &script,
    );
    assert_eq!(
        page.lines().next().unwrap(),
        "…t-x86_64-nodebug.elf › __rust_dealloc › Handle<NodeRef<…>, KV>::split::<Global>"
    );
}

#[test]
fn enter_on_a_caller_opens_its_callers_with_its_code_at_the_call() {
    // A link opens a page on the same tab, so Enter after Enter walks up
    // the callers: from log_msg to process_frame, then to main.
    let script = ["3", "Down", "Down", "Down", "Enter"];
    assert_output(
        &screen("c-x86_64.elf", Some("log_msg"), (100, 30), &script),
        "\
c-x86_64.elf › log_msg › process_frame
process_frame  function at 0x1700, 532 bytes, in .text
used by 1 · uses 11
 1 code   2 callees  [3 callers]  4 reach
  HOW   AT                 CALLER
> call  0x10f2 and 2 more  main





















→ main  function at 0x10a0, 195 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf callers process_frame
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen(
            "c-x86_64.elf",
            Some("log_msg"),
            (100, 30),
            &[&script[..], &["Enter"]].concat(),
        ),
        "\
c-x86_64.elf › log_msg › process_frame › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
 1 code   2 callees  [3 callers]  4 reach
  HOW            AT      CALLER
> address taken  0x1188  _start




















Calls to main through a pointer aren't listed, since elfq can't see them.
→ _start  function at 0x1170, 38 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf callers main
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    // process_frame calls log_msg at 0x18ee and 0x1908; the first is
    // selected on its code tab.
    assert_output(
        &screen(
            "c-x86_64.elf",
            Some("log_msg"),
            (100, 30),
            &[&script[..], &["1"]].concat(),
        ),
        "\
c-x86_64.elf › log_msg › process_frame
process_frame  function at 0x1700, 532 bytes, in .text
used by 1 · uses 11
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
  0x18b3   pop     r13
  0x18b5   pop     r14
  0x18b7   pop     r15
  0x18b9   ret
  0x18ba   nop     [rax+rax]
  0x18c0   mov     ebx, 0x20
  0x18c5   jmp     0x1778                      jump ↑
  0x18ca   nop     [rax+rax]
  0x18d0   xor     esi, esi
  0x18d2   lea     rdi, [rsp+0x10]
  0x18d7   call    apply_filter                call
  0x18dc   xor     esi, esi
  0x18de   jmp     0x1873                      jump ↑
  0x18e0   lea     rdi, [rip+0x72b]            uses .rodata@0x2012 \"bad header\\n\"
  0x18e7   add     dword ptr [rip+0x2766], 1   uses frames_bad
> 0x18ee   call    log_msg                     call
  0x18f3   mov     ebp, 0xffffffff
  0x18f8   jmp     0x1899                      jump ↑
  0x18fa   lea     rdi, [rip+0x71d]            uses .rodata@0x201e \"bad crc\\n\"
  0x1901   add     dword ptr [rip+0x274c], 1   uses frames_bad
  0x1908   call    log_msg                     call
  0x190d   jmp     0x18f3                      jump ↑
  0x190f   call    __stack_chk_fail@plt        call
→ log_msg  function at 0x1610, 51 bytes, in .text
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    // Esc comes back to the row, on the callers tab.
    let back = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (100, 30),
        &[&script[..], &["Esc"]].concat(),
    );
    assert_eq!(
        back,
        screen("c-x86_64.elf", Some("log_msg"), (100, 30), &script[..4])
    );
    // Tab goes from row to row, round the end.
    let page = screen(
        "c-x86_64.elf",
        Some("log_msg"),
        (100, 30),
        &["3", "BackTab"],
    );
    let (selected, _) = selected_and_status(&page);
    assert_eq!(selected, "> call       0x1a72          report");
}

#[test]
fn each_page_keeps_its_tab_and_each_tab_its_row() {
    let screen = |script: &[&str]| screen("c-x86_64.elf", Some("log_msg"), (80, 24), script);
    // The callers tab keeps its row while the code is on the screen.
    assert_eq!(
        screen(&["3", "Down", "Down", "1", "3"]),
        screen(&["3", "Down", "Down"])
    );
    // So do the callees and reach tabs.
    for tab in ["2", "4"] {
        assert_eq!(
            screen(&[tab, "Down", "Down", "1", "3", tab]),
            screen(&[tab, "Down", "Down"])
        );
    }
    // Forward brings a page back on the tab it was left on.
    let script = ["3", "Down", "Down", "Down", "Enter", "1"];
    assert_eq!(
        screen(&[&script[..], &["Esc", "]"]].concat()),
        screen(&script)
    );
}

#[test]
fn two_lists_what_the_callees_command_lists() {
    assert_output(
        &screen("c-x86_64.elf", Some("main"), (100, 30), &["2"]),
        "\
c-x86_64.elf › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
 1 code  [2 callees]  3 callers   4 reach
  HOW            AT                 CALLEE
> address taken  0x10a8             on_timer
  call           0x10b1 and 2 more  register_handler
  address taken  0x10b6             on_button
  address taken  0x10c7             on_data
  uses           0x10da             .rodata@0x205e
  call           0x10e1             parse_value
  uses           0x10eb             frame_a
  call           0x10f2 and 2 more  process_frame
  uses           0x10fc             frame_b
  uses           0x110d             frame_c
  call           0x111b and 4 more  app_tick
  call           0x1152             dispatch_event
  call           0x1157             report









→ on_timer  function at 0x1260, 11 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf callees main
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    // A data object's callees are what it holds the addresses of.
    assert_output(
        &screen("cpp-x86_64.elf", Some("vtable for Gain"), (80, 24), &["2"]),
        "\
cpp-x86_64.elf › vtable for Gain
vtable for app::Gain  data object at 0x3d38, 24 bytes, in .data.rel.ro
used by 1 · uses 2
[2 callees]  3 callers   4 reach
  HOW      AT      CALLEE
> pointer  0x3d40  typeinfo for app::Gain
  pointer  0x3d48  app::Gain::run(short*, unsigned long)














→ typeinfo for app::Gain  data object at 0x3d78, 24 bytes, in .data.rel.ro
$ elfq testdata/bin/cpp-x86_64.elf callees 'vtable for Gain'
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}

#[test]
fn enter_on_a_callee_opens_its_callees_and_esc_comes_back() {
    let screen = |script: &[&str]| screen("c-x86_64.elf", Some("main"), (80, 24), script);
    let script = ["2", "Down", "Down", "Down", "Down", "Down", "Enter"];
    assert_output(
        &screen(&script),
        "\
c-x86_64.elf › main › parse_value
parse_value  function at 0x1460, 84 bytes, in .text
used by 2 · uses 2
 1 code  [2 callees]  3 callers   4 reach
  HOW        AT      CALLEE
> uses       0x149a  config_sum
  tail call  0x14a8  parse_list














→ config_sum  data object at 0x4050, 4 bytes, in .bss
$ elfq testdata/bin/c-x86_64.elf callees parse_value
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // Esc comes back to the row that was selected.
    assert_eq!(
        screen(&[&script[..], &["Esc"]].concat()),
        screen(&script[..script.len() - 1])
    );
}

#[test]
fn four_lists_what_the_reach_command_lists() {
    // Firmware's blocks of memory come before the rows.
    assert_output(
        &screen("c-cortex-m7.elf", Some("main"), (100, 30), &["4"]),
        "\
c-cortex-m7.elf › main
main  function at 0x30000570, 132 bytes, in .text (0x30000000..0x30000edc)
used by 1 · uses 13
 1 code   2 callees   3 callers  [4 reach]
main and the 55 items it reaches take 2036 bytes of code and 5431 bytes of data; elfq can't follow 1
jump:
  MEMORY                  CODE  DATA  SECTIONS
  0x00000000..0x000000e0   200     0  .itcm
  0x20000000..0x20000450     0  1100  .data .bss
  0x20240000..0x20241000     0  4096  .ocram_bss
  0x30000000..0x30000edc  1836   235  .text .rodata
  ADDRESS     SIZE  HOW              ITEM
> 0x30000570   132                   main
  0x74          92  call via veneer  apply_filter, used by process_frame
  0x40          52  call             filter_tap, used by apply_filter
  0x10          48  call via veneer  crc32, used by process_frame
  0xd8           8  call             __memcpy_veneer, used by apply_filter
  0x20000000  1024  uses             crc_table, used by crc32
  0x20000400    32  uses             stats, used by process_frame
  0x20000438    12  uses             handlers, used by register_handler
  0x20000430     4  uses             button_presses, used by on_button
  0x20000420     4  uses             config_sum, used by parse_value
  0x2000042c     4  uses             data_events, used by on_data
  0x20000424     4  uses             frames_bad, used by process_frame
  0x20000428     4  uses             frames_ok, used by process_frame
dispatch_event also jumps through a pointer, at 0x300002da, which elfq can't follow, so main may
reach more.
→ main  function at 0x30000570, 132 bytes, in .text
$ elfq testdata/bin/c-cortex-m7.elf reach main --all
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.so", Some("log_msg"), (80, 24), &["4", "End"]),
        "\
c-x86_64.so › log_msg
log_msg  function at 0x16c0, 51 bytes, in .text
used by 5 · uses 2
 1 code   2 callees   3 callers  [4 reach]
log_msg and the 4 functions it reaches take 119 bytes of code and 0 bytes of
data, plus 2 imports:
  ADDRESS  SIZE  HOW                ITEM
  0x16c0     51                     log_msg
  0x1b70     20  tail call via PLT  platform_write, used by log_msg
  0x1150     16  tail call          platform_write@plt, used by log_msg
  0x10f0     16  call               strlen@plt, used by log_msg
  0x10e0     16  tail call          write@plt, used by platform_write
                 tail call          strlen, used by strlen@plt
>                tail call          write, used by write@plt







→ write@plt  function at 0x10e0, 16 bytes, in .plt.sec
$ elfq testdata/bin/c-x86_64.so reach log_msg
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}

#[test]
fn enter_on_a_row_of_reach_opens_what_that_item_reaches_and_esc_comes_back() {
    let x86 = |script: &[&str]| screen("c-x86_64.elf", Some("main"), (80, 24), script);
    let script = ["4", "Down", "Enter"];
    assert_output(
        &x86(&script),
        "\
c-x86_64.elf › main › process_frame
process_frame  function at 0x1700, 532 bytes, in .text
used by 1 · uses 11
 1 code   2 callees   3 callers  [4 reach]
process_frame and the 18 items it reaches take 1143 bytes of code and 1141 bytes
of data, plus 3 imports; elfq can't follow 1 jump:
  ADDRESS  SIZE  HOW        ITEM
> 0x1700    532             process_frame
  0x1350    271  call       apply_filter, used by process_frame
  0x1300     75  call       filter_tap, used by apply_filter
  0x1280     59  call       crc32, used by process_frame
  0x12c0     54  call       decode_header, used by process_frame
  0x1610     51  call       log_msg, used by process_frame
  0x1690     33  call       dispatch_event, used by process_frame
  0x1ac0     20  tail call  platform_write, used by log_msg
  0x1090     16  call       __stack_chk_fail@plt, used by process_frame
  0x1080     16  call       strlen@plt, used by log_msg
  0x1070     16  tail call  write@plt, used by platform_write
  0x2120   1024  uses       crc_table, used by crc32
dispatch_event also jumps through a pointer, at 0x16ad, which elfq can't follow,
so process_frame may reach more.
→ process_frame  function at 0x1700, 532 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf reach process_frame
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    assert_eq!(
        x86(&[&script[..], &["Esc"]].concat()),
        x86(&script[..script.len() - 1])
    );
    // The first row, the page's own item, opens no other page.
    let own = x86(&["4", "Enter"]);
    let (selected, status) = selected_and_status(&own);
    assert!(own.starts_with("c-x86_64.elf › main\n"), "{own}");
    assert!(selected.ends_with(" main"), "{own}");
    assert_eq!(status, "this is main's page", "{own}");
    // The row of a call to code outside every item opens the item that
    // makes the call.
    let sample = "far-gnu-m4-nodebug.elf";
    let printed = elfq(&[
        &format!("testdata/bin/{sample}"),
        "reach",
        "Reset_Handler",
        "--all",
    ]);
    let printed = String::from_utf8(printed.stdout).unwrap();
    let rows = printed
        .lines()
        .skip_while(|line| !line.starts_with("ADDRESS"));
    let row = rows
        .skip(1)
        .position(|line| line.contains("(outside any function), used by "));
    let script = [&["4"][..], &vec!["Down"; row.unwrap()]].concat();
    let page = screen(sample, Some("Reset_Handler"), (100, 30), &script);
    let (selected, status) = selected_and_status(&page);
    assert!(
        selected.contains("(outside any function), used by ram_calls_rom"),
        "{page}"
    );
    assert!(
        status.starts_with("→ ram_calls_rom  function at "),
        "{page}"
    );
    let opened = screen(
        sample,
        Some("Reset_Handler"),
        (100, 30),
        &[&script[..], &["Enter"]].concat(),
    );
    let title = opened.lines().next().unwrap();
    assert_eq!(
        title,
        format!("{sample} › Reset_Handler › ram_calls_rom"),
        "{opened}"
    );
    // On the same tab.
    assert!(
        opened.lines().any(|line| line.ends_with("[4 reach]")),
        "{opened}"
    );
}

#[test]
fn the_reach_tab_on_a_small_screen_keeps_its_selected_row() {
    assert_output(
        &screen(
            "c-cortex-m7.elf",
            Some("main"),
            (60, 16),
            &["4", "Down", "Down"],
        ),
        "\
c-cortex-m7.elf › main
main  function at 0x30000570, 132 bytes, in .text
(0x30000000..0x30000edc)
used by 1 · uses 13
 1 code   2 callees   3 callers  [4 reach]
main and the 55 items it reaches take 2036 bytes of code and
5431 bytes of data; elfq can't follow 1 jump: …
  ADDRESS     SIZE  HOW         ITEM
  0x30000570   132              main
  0x74          92  call via …  apply_filter, used by proce…
> 0x40          52  call        filter_tap, used by apply_f…
dispatch_event also jumps through a pointer, at 0x300002da,
which elfq can't follow, so main may reach more.
→ filter_tap  function at 0x40, 52 bytes, in .itcm
$ elfq testdata/bin/c-cortex-m7.elf reach main --all
↑↓ move  enter follow  esc back  g go to  ? help  q quit
",
    );
}

#[test]
fn tabs_that_an_item_cant_have_say_why_as_their_commands_do() {
    let cases = [
        // A function whose end elfq doesn't know.
        ("c-x86_64.elf", "_init"),
        // An import with no PLT stub, which has a page of its own.
        ("c-x86_64.elf", "__libc_start_main"),
    ];
    for (sample, symbol) in cases {
        let path = format!("testdata/bin/{sample}");
        for (key, command) in [("2", "callees"), ("4", "reach")] {
            let page = screen(sample, Some(symbol), (100, 30), &[key]);
            let (_, status) = selected_and_status(&page);
            let output = elfq(&[&path, command, symbol]);
            assert!(!output.status.success());
            let error = String::from_utf8(output.stderr).unwrap();
            assert_eq!(
                format!("error: {status}\n"),
                error,
                "{sample} {symbol} {key}"
            );
            // The tab isn't drawn.
            assert!(!page.contains(&format!(" {command} ")), "{page}");
        }
    }
}

/// The command under a page, which may take several lines.
fn command_under(screen: &str) -> String {
    let lines: Vec<&str> = screen.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.starts_with("$ elfq "))
        .unwrap();
    lines[first..lines.len() - 1].join(" ")
}

#[test]
#[cfg(unix)]
fn the_command_under_the_callers_runs_in_a_shell() {
    // Names with spaces, brackets and quotes, which the command quotes.
    let pages = [
        ("cpp-x86_64.elf", "log(unsigned int)", "'log(unsigned int)'"),
        ("cpp-x86_64.elf", "vtable for Gain", "'vtable for Gain'"),
        ("cpp-x86_64.so", "log(unsigned int)", "'log(unsigned int)'"),
        (
            "rust-x86_64-nodebug.elf",
            "0x1e260",
            r"'drop_glue::<alloc::boxed::Box<dyn for<'\''a, '\''b> core::ops::function::Fn<(&'\''a std::panic::PanicHookInfo<'\''b>,), Output = ()> + core::marker::Sync + core::marker::Send>>'",
        ),
    ];
    for (sample, symbol, quoted) in pages {
        let page = screen(sample, Some(symbol), (100, 30), &["3"]);
        let command = command_under(&page);
        assert_eq!(
            command,
            format!("$ elfq testdata/bin/{sample} callers {quoted}")
        );
        // Run by the shell, as when pasted, it lists the page's rows.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("elfq() {{ \"$ELFQ\" \"$@\"; }}; {}", &command[2..]))
            .env("ELFQ", env!("CARGO_BIN_EXE_elfq"))
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        let printed = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{command}: {printed}");
        let rows: Vec<&str> = page
            .lines()
            .skip_while(|line| !line.starts_with("  HOW "))
            .take_while(|line| line.starts_with("  ") || line.starts_with("> "))
            .map(|line| &line[2..])
            .collect();
        let printed_rows: Vec<&str> = printed
            .lines()
            .skip_while(|line| !line.starts_with("HOW "))
            .take_while(|line| !line.is_empty())
            .map(str::trim_end)
            .collect();
        assert!(rows.len() > 1, "{page}");
        assert_eq!(rows, printed_rows, "{command}");
        // The commands under the callees and reach tabs run too.
        for (key, name) in [("2", "callees"), ("4", "reach")] {
            let page = screen(sample, Some(symbol), (100, 30), &[key]);
            let command = command_under(&page);
            let (pasted, all) = match command.strip_suffix(" --all") {
                Some(pasted) => (pasted, " --all"),
                None => (command.as_str(), ""),
            };
            let quoted = format!("$ elfq testdata/bin/{sample} {name} {quoted}");
            assert_eq!(pasted, quoted);
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("elfq() {{ \"$ELFQ\" \"$@\"; }}; {}", &command[2..]))
                .env("ELFQ", env!("CARGO_BIN_EXE_elfq"))
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{command}{all}");
            assert!(!output.stdout.is_empty(), "{command}");
        }
    }
}

#[test]
fn an_import_opens_as_its_stub_or_on_a_page_of_its_own() {
    // Go to lists imports, and one with a PLT stub as the stub.
    assert_output(
        &screen(
            "rust-x86_64-nodebug.elf",
            Some("main"),
            (80, 24),
            &["g", "cxa_"],
        ),
        "\
rust-x86_64-nodebug.elf › main › go to
  ADDRESS  SIZE  KIND      SECTION  ITEM
> 0x51ca0    16  function  .plt     __cxa_finalize@plt
                 import             __cxa_thread_atexit_impl


















go to: cxa_▏
↑↓ move  enter open  esc cancel
",
    );
    let page = screen(
        "rust-x86_64-nodebug.elf",
        Some("main"),
        (80, 24),
        &["g", "memcpy", "Enter"],
    );
    assert_eq!(
        page.lines().take(4).collect::<Vec<_>>(),
        [
            "rust-x86_64-nodebug.elf › main › memcpy",
            "memcpy  imported from a shared library: its code isn't in this file",
            "used by 57",
            "[3 callers]",
        ]
    );
    // Its page has no code, and 1 says why.
    let page = screen("rust-x86_64-nodebug.elf", Some("memcpy"), (80, 24), &["1"]);
    let (_, status) = selected_and_status(&page);
    assert_eq!(
        status,
        "memcpy is imported from a shared library, so its code isn't in this file"
    );
    // A call through the global offset table, as code built with -fno-plt
    // makes, opens the import's page.
    let page = screen("c-x86_64.elf", Some("_start"), (80, 24), &["Tab", "Tab"]);
    assert_eq!(
        selected_and_status(&page),
        (
            "> 0x118f   call    qword ptr [rip+0x2e3b]   call __libc_start_main",
            "→ __libc_start_main  imported from a shared library"
        )
    );
    assert_output(
        &screen(
            "c-x86_64.elf",
            Some("_start"),
            (80, 24),
            &["Tab", "Tab", "Enter"],
        ),
        "\
c-x86_64.elf › _start › __libc_start_main
__libc_start_main  imported from a shared library: its code isn't in this file
used by 1
[3 callers]
  HOW   AT      CALLER
> call  0x118f  _start















→ _start  function at 0x1170, 38 bytes, in .text
$ elfq testdata/bin/c-x86_64.elf callers __libc_start_main
↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
    // And when the import has a stub, the stub's page, as its name finds
    // the stub as SYMBOL. On the stub's page, its jump to the import goes
    // nowhere else.
    let page = screen(
        "rust-x86_64-nodebug.elf",
        Some("__do_global_dtors_aux"),
        (80, 24),
        &["Tab", "Tab"],
    );
    assert_eq!(
        selected_and_status(&page),
        (
            "> 0x1426e  cmp     qword ptr [rip+0x40892], 0  address taken __cxa_finalize",
            "→ __cxa_finalize@plt  function at 0x51ca0, 16 bytes, in .plt"
        )
    );
    let page = screen(
        "rust-x86_64-nodebug.elf",
        Some("__do_global_dtors_aux"),
        (80, 24),
        &["Tab", "Tab", "Enter", "Enter"],
    );
    assert!(
        page.starts_with("rust-x86_64-nodebug.elf › __do_global_dtors_aux › __cxa_finalize@plt\n"),
        "{page}"
    );
    assert_eq!(
        selected_and_status(&page),
        (
            "> 0x51ca0  jmp     qword ptr [rip+0x36d2]  tail call __cxa_finalize",
            "__cxa_finalize is imported from a shared library, so its code isn't in this file"
        )
    );
}

#[test]
fn slash_finds_text_in_the_rows_and_n_and_shift_n_go_round_them() {
    let screen = |script: &[&str]| screen("c-x86_64.elf", Some("main"), (100, 30), script);
    // The prompt, on the status line.
    assert_output(
        &screen(&["/", "Call"]),
        "\
c-x86_64.elf › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
[1 code]  2 callees   3 callers   4 reach
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
  0x10fc   lea     rdi, [rip+0xf8d]  uses frame_b
  0x1103   call    process_frame     call
  0x1108   mov     esi, 5
  0x110d   lea     rdi, [rip+0xf6c]  uses frame_c
  0x1114   call    process_frame     call
find: Call▏
enter find  esc cancel
",
    );
    // Enter selects the first line that has the text, ignoring case, from
    // the selected one on, and says which of how many it is.
    assert_output(
        &screen(&["/", "Call", "Enter"]),
        "\
c-x86_64.elf › main
main  function at 0x10a0, 195 bytes, in .text
used by 1 · uses 13
[1 code]  2 callees   3 callers   4 reach
  ADDRESS  INSTRUCTION
  0x10a0   endbr64
  0x10a4   sub     rsp, 8
  0x10a8   lea     rsi, [rip+0x1b1]  address taken on_timer
  0x10af   xor     edi, edi
> 0x10b1   call    register_handler  call
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
  0x10fc   lea     rdi, [rip+0xf8d]  uses frame_b
  0x1103   call    process_frame     call
  0x1108   mov     esi, 5
  0x110d   lea     rdi, [rip+0xf6c]  uses frame_c
  0x1114   call    process_frame     call
1 of 14 lines with \"Call\"
↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    let found = |script: &[&str]| {
        let page = screen(&[&["/", "call", "Enter"][..], script].concat());
        let (selected, status) = selected_and_status(&page);
        (selected.to_string(), status.to_string())
    };
    let (first, status) = found(&[]);
    assert!(first.starts_with("> 0x10b1 "), "{first}");
    let count: usize = status.split(' ').nth(2).unwrap().parse().unwrap();
    assert_eq!(status, format!("1 of {count} lines with \"call\""));
    // n goes to the next, and N back.
    let (second, status) = found(&["n"]);
    assert_eq!(status, format!("2 of {count} lines with \"call\""));
    assert_ne!(second, first);
    assert_eq!(found(&["n", "N"]), found(&[]));
    // Both go round the end.
    let (last, status) = found(&["N"]);
    assert_eq!(status, format!("{count} of {count} lines with \"call\""));
    assert_eq!(found(&["N", "n"]).0, first);
    assert_ne!(last, first);
    // Enter finds from the selected line on: the selected one, if it has
    // the text.
    assert_eq!(
        selected_and_status(&screen(&["/", "call", "Enter", "/", "call", "Enter"])).0,
        first
    );
    // A text that no line has leaves the cursor where it was.
    let page = screen(&["Down", "/", "zzz", "Enter"]);
    assert_eq!(
        selected_and_status(&page),
        ("> 0x10a4   sub     rsp, 8", "no line has \"zzz\"")
    );
    // Esc closes the prompt, and leaves the page as it was.
    assert_eq!(screen(&["Down", "/", "cal", "Esc"]), screen(&["Down"]));
    // n and N need a text found first.
    let page = screen(&["n"]);
    assert_eq!(selected_and_status(&page).1, "press / to find text first");
    // A tab's rows are found by every column, with names in full.
    let page = screen(&["3", "/", "_START", "Enter"]);
    assert_eq!(
        selected_and_status(&page),
        (
            "> address taken  0x1188  _start",
            "the only row with \"_START\""
        )
    );
}

#[test]
fn find_has_no_rows_on_the_help_and_ctrl_c_and_ctrl_z_work_while_typing() {
    let page = screen("c-x86_64.elf", Some("main"), (80, 24), &["?", "/"]);
    let (_, status) = selected_and_status(&page);
    assert_eq!(status, "this page has no rows to find in");
    // Nor does a tab whose table has none, and its hint line doesn't name
    // find.
    let page = screen("c-x86_64.elf", Some("_start"), (100, 30), &["3", "/"]);
    let (_, status) = selected_and_status(&page);
    assert_eq!(status, "this page has no rows to find in", "{page}");
    assert!(!page.contains("/ find"), "{page}");
    let binary = load_sample("c-x86_64.elf");
    let stop = |script: &[&str]| browse(SAMPLE, &binary, None, (80, 24), script).1;
    // q types into the prompt.
    let page = screen("c-x86_64.elf", None, (80, 24), &["/", "q"]);
    assert!(page.contains("find: q▏"), "{page}");
    assert_eq!(stop(&["/", "Ctrl-C"]), Stop::Quit);
    #[cfg(unix)]
    assert_eq!(stop(&["/", "Ctrl-Z"]), Stop::Suspend);
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
        assert_eq!(lines[4], "  ADDRESS  INSTRUCTION", "{script:?}");
        assert!(lines[5].starts_with("> 0x10f7 "), "{script:?}\n{page}");
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
    let mut app = App::new(session, Some("log_msg"));
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
        .map(|column| terminal.backend().buffer()[(column, 5)].symbol())
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
    let mut app = App::new(session, None);
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
#[cfg(target_os = "linux")]
fn the_start_page_is_drawn_in_a_terminal() {
    let Some((status, output)) = in_a_terminal(&["testdata/bin/c-cortex-m7.elf", "browse"], "q")
    else {
        eprintln!("no `script` to run elfq in a terminal");
        return;
    };
    assert_eq!(status, Some(0), "{output:?}");
    // The table of starts, and the screen given back.
    assert!(output.contains("START"), "{output:?}");
    assert!(output.contains("\x1b[?1049l"), "{output:?}");
}

#[test]
fn go_to_says_why_it_lists_nothing() {
    // In the CLI's words.
    assert_output(
        &screen("cpp-x86_64.elf", Some("main"), (80, 24), &["g", "mainz"]),
        "\
cpp-x86_64.elf › main › go to
  ADDRESS  SIZE  KIND  SECTION  ITEM
no function or data object is named `mainz`; did you mean main?



















go to: mainz▏
↑↓ move  enter open  esc cancel
",
    );
}

#[test]
fn go_to_names_what_symbol_finds_as_the_commands_do() {
    // Another name of an item finds it, and its row and its page say so.
    assert_output(
        &screen(
            "c-cortex-m7.elf",
            Some("main"),
            (100, 16),
            &["g", "NMI_Handler"],
        ),
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
        Some("main"),
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
        screen("c-cortex-m7.elf", Some("main"), (100, 16), &["g", "CRC"]),
        screen("c-cortex-m7.elf", Some("main"), (100, 16), &["g", "crc"])
            .replace("go to: crc", "go to: CRC")
    );
    // It lists 500 items, the largest first, and Home and End go to the
    // first and the last.
    assert_output(
        &screen(
            "rust-x86_64-nodebug.elf",
            Some("main"),
            (100, 16),
            &["g", "e", "End", "Home"],
        ),
        "\
rust-x86_64-nodebug.elf › main › go to
  ADDRESS   SIZE  KIND      SECTION             ITEM
> 0x18c00  15332  function  .text               <Cache>::with_global::<resolve::{closure#1}>
  0x336a3  12242  function  .text               <std::backtrace_rs::symbolize::gimli::Context>::new
  0x2f760   9301  function  .text               <Unit<EndianSlice<LittleEndian>, usize>>::new
  0x16b20   8244  function  .text               <OnceCell<Result<Lines, Error>>>::try_init::<get_or…
  0x49d5d   6210  function  .text               miniz_oxide::inflate::core::decompress
  0x20b10   5196  function  .text               unit::parse_attribute::<EndianSlice<LittleEndian>>
  0x2d960   4538  function  .text               <Function<EndianSlice<…>>>::parse_children
  0x3c8c0   4161  function  .text               thread_info::set_current_info
  0x37c50   3972  function  .text               <RngListIter<EndianSlice<LittleEndian>>>::next
  0x15ba0   3964  function  .text               <OnceCell<Result<Functions<EndianSlice<LittleEndian…
  0x2b940   3222  function  .text               <Mapping>::new_debug
and 433 more; type more of the name
go to: e▏
↑↓ move  enter open  esc cancel
",
    );
    let end = screen(
        "rust-x86_64-nodebug.elf",
        Some("main"),
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
cpp-x86_64.elf › go to
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
    let back = screen("c-x86_64.elf", Some("main"), (80, 24), &["?", "?", "Esc"]);
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
[1 code]  2 callees   3 callers   4 reach
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












↑↓ move  enter follow  esc back  tab next link  1 2 3 4 tabs  / find  g go to  ? help  q quit
",
    );
    assert_output(
        &screen("c-x86_64.so", Some("app_tick"), (80, 24), &[]),
        "\
c-x86_64.so › app_tick
app_tick  function at 0x1770, 62 bytes, in .text
used by 1 · uses 3
[1 code]  2 callees   3 callers   4 reach
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

↑↓ move  enter follow  esc back  tab next link  / find  g go to  ? help  q quit
",
    );
}
