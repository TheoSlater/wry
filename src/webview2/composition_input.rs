use std::{
  cell::Cell,
  sync::atomic::{AtomicUsize, Ordering},
};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::core::Interface;
use windows::Win32::{
  Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
  Graphics::Gdi::ScreenToClient,
  UI::{
    Input::KeyboardAndMouse::{SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
    WindowsAndMessaging::*,
  },
};

use crate::{HitTestMode, Rect};

static NEXT_SUBCLASS_ID: AtomicUsize = AtomicUsize::new(0x1000);
const WM_MOUSELEAVE_MESSAGE: u32 = 0x02a3;

pub(crate) struct InputHandle {
  parent: HWND,
  id: usize,
  state: Box<InputState>,
}

struct InputState {
  controller: ICoreWebView2CompositionController,
  bounds: Cell<RECT>,
  mode: Cell<HitTestMode>,
  visible: Cell<bool>,
  hovered: Cell<bool>,
  auto_resize: bool,
}

impl InputHandle {
  pub(crate) unsafe fn install(
    parent: HWND,
    controller: ICoreWebView2CompositionController,
    bounds: RECT,
    mode: HitTestMode,
    auto_resize: bool,
  ) -> Self {
    let id = NEXT_SUBCLASS_ID.fetch_add(1, Ordering::Relaxed);
    let mut state = Box::new(InputState {
      controller,
      bounds: Cell::new(bounds),
      mode: Cell::new(mode),
      visible: Cell::new(true),
      hovered: Cell::new(false),
      auto_resize,
    });
    // Safety: Box keeps state at stable address until Drop removes subclass.
    let _ = SetWindowSubclass(
      parent,
      Some(input_subclass_proc),
      id,
      (&mut *state as *mut InputState) as usize,
    );
    Self { parent, id, state }
  }

  pub(crate) fn set_bounds(&self, bounds: RECT) {
    self.state.bounds.set(bounds);
  }

  pub(crate) fn set_mode(&self, mode: HitTestMode) {
    self.state.mode.set(mode);
  }

  pub(crate) fn set_visible(&self, visible: bool) {
    self.state.visible.set(visible);
  }
}

impl Drop for InputHandle {
  fn drop(&mut self) {
    unsafe {
      let _ = RemoveWindowSubclass(self.parent, Some(input_subclass_proc), self.id);
    }
  }
}

unsafe extern "system" fn input_subclass_proc(
  hwnd: HWND,
  msg: u32,
  wparam: WPARAM,
  lparam: LPARAM,
  _id: usize,
  ref_data: usize,
) -> LRESULT {
  // Safety: ref_data is InputHandle's Box pointer; subclass removed before Box drop.
  let state = &mut *(ref_data as *mut InputState);
  if matches!(msg, WM_SIZE | WM_DPICHANGED) && state.auto_resize {
    let mut rect = RECT::default();
    let _ = GetClientRect(hwnd, &mut rect);
    state.bounds.set(rect);
    let _ = state
      .controller
      .cast::<ICoreWebView2Controller>()
      .and_then(|controller| unsafe { controller.SetBounds(rect) });
  }
  if matches!(msg, WM_MOVE | WM_MOVING | WM_ENTERSIZEMOVE) {
    let _ = state
      .controller
      .cast::<ICoreWebView2Controller>()
      .and_then(|controller| unsafe { controller.NotifyParentWindowPositionChanged() });
  }
  if !state.visible.get() || state.mode.get() == HitTestMode::Passthrough {
    return DefSubclassProc(hwnd, msg, wparam, lparam);
  }

  if msg == WM_SETCURSOR {
    let mut cursor = HCURSOR::default();
    if state.controller.Cursor(&mut cursor).is_ok() {
      SetCursor(Some(cursor));
      return LRESULT(1);
    }
  }

  let kind = match msg {
    WM_MOUSEMOVE => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE),
    WM_LBUTTONDOWN => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN),
    WM_LBUTTONUP => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP),
    WM_RBUTTONDOWN => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN),
    WM_RBUTTONUP => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP),
    WM_MBUTTONDOWN => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN),
    WM_MBUTTONUP => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP),
    WM_MOUSEWHEEL => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL),
    WM_MOUSEHWHEEL => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL),
    WM_MOUSELEAVE_MESSAGE => Some(COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE),
    WM_SETFOCUS => {
      let _ = state
        .controller
        .cast::<ICoreWebView2Controller>()
        .and_then(|controller| unsafe {
          controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)
        });
      None
    }
    _ => None,
  };

  if let Some(kind) = kind {
    let mut point = if msg == WM_MOUSELEAVE_MESSAGE {
      POINT { x: 0, y: 0 }
    } else {
      POINT {
        x: (lparam.0 as i16) as i32,
        y: ((lparam.0 >> 16) as i16) as i32,
      }
    };
    if matches!(msg, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
      let _ = ScreenToClient(hwnd, &mut point);
    }
    if msg != WM_MOUSELEAVE_MESSAGE {
      let bounds = state.bounds.get();
      if point.x < bounds.left
        || point.y < bounds.top
        || point.x >= bounds.right
        || point.y >= bounds.bottom
      {
        if state.hovered.replace(false) {
          let _ = state.controller.SendMouseInput(
            COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
            COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
            0,
            POINT { x: 0, y: 0 },
          );
        }
        return DefSubclassProc(hwnd, msg, wparam, lparam);
      }
      state.hovered.set(true);
      point.x -= bounds.left;
      point.y -= bounds.top;
    }
    let keys = virtual_keys(wparam.0 as u32 & 0xffff);
    let data = if matches!(msg, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
      ((wparam.0 >> 16) as i16 as i32) as u32
    } else {
      0
    };
    let _ = state.controller.SendMouseInput(kind, keys, data, point);
    if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN) {
      let _ = SetFocus(Some(hwnd));
      let _ = state
        .controller
        .cast::<ICoreWebView2Controller>()
        .and_then(|controller| controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC));
    }
    if msg == WM_MOUSEMOVE {
      let mut tracking = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
      };
      let _ = TrackMouseEvent(&mut tracking);
    }
  }

  DefSubclassProc(hwnd, msg, wparam, lparam)
}

pub(crate) fn rect(bounds: Rect, scale_factor: f64) -> RECT {
  let position = bounds.position.to_physical::<i32>(scale_factor);
  let size = bounds.size.to_physical::<i32>(scale_factor);
  RECT {
    left: position.x,
    top: position.y,
    right: position.x + size.width,
    bottom: position.y + size.height,
  }
}

fn virtual_keys(flags: u32) -> COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS {
  let mut keys = COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE;
  if flags & 1 != 0 {
    keys = keys | COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON;
  }
  if flags & 2 != 0 {
    keys = keys | COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON;
  }
  if flags & 16 != 0 {
    keys = keys | COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON;
  }
  if flags & 4 != 0 {
    keys = keys | COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT;
  }
  if flags & 8 != 0 {
    keys = keys | COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL;
  }
  keys
}

#[cfg(test)]
mod tests {
  use super::*;
  use dpi::{LogicalPosition, LogicalSize};

  #[test]
  fn converts_logical_rect_to_physical_rect() {
    let rect = rect(
      Rect {
        position: LogicalPosition::new(10.0, 20.0).into(),
        size: LogicalSize::new(100.0, 50.0).into(),
      },
      1.5,
    );
    assert_eq!(rect.left, 15);
    assert_eq!(rect.top, 30);
    assert_eq!(rect.right, 165);
    assert_eq!(rect.bottom, 105);
  }

  #[test]
  fn converts_mouse_flags() {
    let keys = virtual_keys(1 | 4 | 8);
    assert!(keys.contains(COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON));
    assert!(keys.contains(COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT));
    assert!(keys.contains(COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL));
  }
}
