use std::{
  cell::{Cell, RefCell},
  collections::HashMap,
  rc::{Rc, Weak},
  sync::mpsc,
};

use webview2_com::{
  CreateCoreWebView2CompositionControllerCompletedHandler, CursorChangedEventHandler,
  Microsoft::Web::WebView2::Win32::*,
};
use windows::{
  core::{IUnknown, Interface},
  Win32::{
    Foundation::HWND,
    Graphics::DirectComposition::{
      DCompositionCreateDevice2, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
    },
    UI::WindowsAndMessaging::{SetCursor, HCURSOR},
  },
};

use crate::{HitTestMode, Rect, Result};

use super::composition_input::{self, InputHandle};

thread_local! {
  static HOSTS: RefCell<HashMap<isize, Weak<CompositionHost>>> = RefCell::new(HashMap::new());
}

pub(crate) struct CompositionHost {
  pub(crate) device: IDCompositionDevice,
  #[allow(dead_code)]
  pub(crate) target: IDCompositionTarget,
  pub(crate) root: IDCompositionVisual,
}

impl CompositionHost {
  fn for_window(hwnd: HWND) -> Result<Rc<Self>> {
    let key = hwnd.0 as isize;
    if let Some(host) = HOSTS.with(|hosts| hosts.borrow().get(&key).and_then(Weak::upgrade)) {
      return Ok(host);
    }

    let host = unsafe {
      // DirectComposition accepts an optional native rendering device; Windows
      // selects its native GPU compositor when none is supplied.
      let device = DCompositionCreateDevice2::<Option<&IUnknown>, IDCompositionDevice>(None)?;
      let target = device.CreateTargetForHwnd(hwnd, true)?;
      let root = device.CreateVisual()?;
      target.SetRoot(&root)?;
      device.Commit()?;
      Rc::new(Self {
        device,
        target,
        root,
      })
    };
    HOSTS.with(|hosts| {
      hosts.borrow_mut().insert(key, Rc::downgrade(&host));
    });
    Ok(host)
  }

  fn add_visual(&self) -> Result<IDCompositionVisual> {
    let visual = unsafe { self.device.CreateVisual()? };
    unsafe {
      self
        .root
        .AddVisual(&visual, false, None::<&IDCompositionVisual>)?;
      self.device.Commit()?;
    }
    Ok(visual)
  }

  fn remove_visual(&self, visual: &IDCompositionVisual) {
    unsafe {
      let _ = self.root.RemoveVisual(visual);
      let _ = self.device.Commit();
    }
  }
}

pub(crate) struct CompositionWebViewController {
  pub(crate) controller: ICoreWebView2Controller,
  pub(crate) composition_controller: ICoreWebView2CompositionController,
  pub(crate) host: Rc<CompositionHost>,
  pub(crate) visual: IDCompositionVisual,
  input: InputHandle,
  bounds: Cell<Rect>,
  cursor_changed_token: i64,
}

impl CompositionWebViewController {
  pub(crate) fn new(
    parent: HWND,
    env: &ICoreWebView2Environment,
    auto_resize: bool,
  ) -> Result<Self> {
    let env3 = env.cast::<ICoreWebView2Environment3>()?;
    let (tx, rx) = mpsc::channel();
    let handler = CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(
      move |error_code, controller| {
        let result = error_code.and_then(|_| {
          controller
            .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_POINTER))
        });
        tx.send(result)
          .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_UNEXPECTED))
      },
    ));
    unsafe {
      env3.CreateCoreWebView2CompositionController(parent, &handler)?;
    }
    let composition_controller = webview2_com::wait_with_pump(rx)??;
    let controller = composition_controller.cast::<ICoreWebView2Controller>()?;
    let host = CompositionHost::for_window(parent)?;
    let visual = host.add_visual()?;
    unsafe {
      composition_controller.SetRootVisualTarget(&visual)?;
    }
    let mut cursor_changed_token = 0;
    let cursor_handler = CursorChangedEventHandler::create(Box::new(move |controller, _| {
      if let Some(controller) = controller {
        let mut cursor = HCURSOR::default();
        unsafe {
          controller.Cursor(&mut cursor)?;
          SetCursor(Some(cursor));
        }
      }
      Ok(())
    }));
    unsafe {
      composition_controller.add_CursorChanged(&cursor_handler, &mut cursor_changed_token)?;
    }
    let input = unsafe {
      InputHandle::install(
        parent,
        composition_controller.clone(),
        windows::Win32::Foundation::RECT::default(),
        HitTestMode::Normal,
        auto_resize,
      )
    };
    #[cfg(any(debug_assertions, feature = "tracing"))]
    {
      #[cfg(feature = "tracing")]
      tracing::debug!("wry: using WebView2 composition controller");
      #[cfg(debug_assertions)]
      eprintln!("wry: using WebView2 composition controller");
    }
    Ok(Self {
      controller,
      composition_controller,
      host,
      visual,
      input,
      bounds: Cell::new(Rect::default()),
      cursor_changed_token,
    })
  }

  pub(crate) fn set_bounds(&self, bounds: Rect, scale_factor: f64) -> Result<()> {
    let rect = composition_input::rect(bounds, scale_factor);
    unsafe {
      self.visual.SetOffsetX2(rect.left as f32)?;
      self.visual.SetOffsetY2(rect.top as f32)?;
      self.host.device.Commit()?;
    }
    self.input.set_bounds(rect);
    self.bounds.set(bounds);
    Ok(())
  }

  pub(crate) fn set_hit_test_mode(&self, mode: HitTestMode) {
    self.input.set_mode(mode);
  }

  pub(crate) fn set_visible(&self, visible: bool) -> Result<()> {
    unsafe {
      self.controller.SetIsVisible(visible)?;
    }
    self.input.set_visible(visible);
    Ok(())
  }

  pub(crate) fn bounds(&self) -> Rect {
    self.bounds.get()
  }

  pub(crate) fn root_visual(&self) -> IDCompositionVisual {
    self.host.root.clone()
  }
  pub(crate) fn device(&self) -> IDCompositionDevice {
    self.host.device.clone()
  }
}

impl Drop for CompositionWebViewController {
  fn drop(&mut self) {
    unsafe {
      let _ = self
        .composition_controller
        .remove_CursorChanged(self.cursor_changed_token);
    }
    self.host.remove_visual(&self.visual);
    // Close here too: initialization can fail after the composition controller
    // exists but before InnerWebView takes ownership of it.
    let _ = unsafe { self.controller.Close() };
  }
}
