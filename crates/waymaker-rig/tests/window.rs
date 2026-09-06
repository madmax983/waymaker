//! Carving one part into an engine area and an instrument area.

use waymaker_flash::storage::{Geometry, StableStorage};
use waymaker_rig::window::{Window, WindowError};

fn part() -> Geometry {
    let Ok(geometry) = Geometry::new(4096, 256, 4, 1) else {
        unreachable!("a legal geometry")
    };
    geometry
}

fn device() -> waymaker_fault::Device {
    waymaker_fault::Device::new(part())
}

#[test]
fn a_window_reports_the_geometry_of_the_area_rather_than_of_the_part() {
    let mut part_view = device();
    let window = Window::new(&mut part_view, 1024, 2048).expect("a legal window");
    assert_eq!(window.geometry().capacity(), 2048);
    assert_eq!(window.geometry().erase_size(), part().erase_size());
    assert_eq!(window.geometry().program_size(), part().program_size());
    assert_eq!(window.geometry().read_size(), part().read_size());
    assert_eq!(window.base(), 1024);
}

#[test]
fn what_is_written_through_a_window_lands_at_the_offset_the_part_uses() {
    let mut part_view = device();
    {
        let mut window = Window::new(&mut part_view, 1024, 2048).expect("a legal window");
        window.erase(0, 256).expect("the window's first block");
        window.program(0, b"\xDE\xAD\xBE\xEF").expect("four bytes");
    }
    let mut page = [0_u8; 4];
    part_view.read(1024, &mut page).expect("a legal read");
    assert_eq!(page, [0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn a_window_cannot_reach_past_its_own_end() {
    let mut part_view = device();
    let mut window = Window::new(&mut part_view, 1024, 512).expect("a legal window");
    window.erase(0, 256).expect("the window's first block");
    // The part has media at 1024 + 512, and the window must not be a way to reach it.
    assert!(window.program(512, b"\x00\x00\x00\x00").is_err());
    assert!(window.erase(512, 256).is_err());
    let mut page = [0_u8; 4];
    assert!(window.read(512, &mut page).is_err());
}

#[test]
fn two_windows_over_one_part_do_not_overlap() {
    let mut part_view = device();
    {
        let mut engine = Window::new(&mut part_view, 0, 2048).expect("a legal window");
        engine.erase(0, 2048).expect("the engine area");
        engine.program(0, b"\x11\x11\x11\x11").expect("a write");
    }
    {
        let mut instrument = Window::new(&mut part_view, 2048, 2048).expect("a legal window");
        instrument.erase(0, 256).expect("the instrument area");
        instrument.program(0, b"\x22\x22\x22\x22").expect("a write");
    }
    let mut page = [0_u8; 4];
    part_view.read(0, &mut page).expect("a legal read");
    assert_eq!(page, [0x11; 4]);
    part_view.read(2048, &mut page).expect("a legal read");
    assert_eq!(page, [0x22; 4]);
}

#[test]
fn a_window_that_is_not_erase_aligned_is_refused() {
    // An erase through a window would otherwise clear a block the window does not own,
    // which is the one mistake a carve-up must not permit.
    assert_eq!(
        Window::new(&mut device(), 128, 1024).unwrap_err(),
        WindowError::Unaligned
    );
    assert_eq!(
        Window::new(&mut device(), 256, 128).unwrap_err(),
        WindowError::Unaligned
    );
}

#[test]
fn a_window_reaching_past_the_part_is_refused() {
    assert_eq!(
        Window::new(&mut device(), 4096, 256).unwrap_err(),
        WindowError::PastTheEnd
    );
    assert_eq!(
        Window::new(&mut device(), 3840, 512).unwrap_err(),
        WindowError::PastTheEnd
    );
}

#[test]
fn a_window_of_no_bytes_is_refused() {
    assert_eq!(
        Window::new(&mut device(), 0, 0).unwrap_err(),
        WindowError::Empty
    );
}

#[test]
fn a_barrier_through_a_window_is_the_parts_barrier() {
    // Deliberately not narrowed. §12's barrier orders everything before it against
    // everything after it on the device, and a window that "scoped" it would be claiming a
    // guarantee no part provides.
    let mut part_view = device();
    let mut window = Window::new(&mut part_view, 0, 1024).expect("a legal window");
    window.barrier().expect("a barrier");
}
