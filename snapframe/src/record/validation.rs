//! GPU-presented animated source for opt-in recording validation. Each displayed
//! frame carries a 16-bit barcode, allowing decoded fresh-motion checks.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use windows::core::{w, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct3D11::{ID3D11DeviceContext1, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory2, DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_EFFECT_FLIP_DISCARD, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

pub struct Animation {
    pub hwnd: isize,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Animation {
    pub fn region(&self) -> crate::capture::Rect {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(HWND(self.hwnd as *mut _), &mut rect).unwrap() };
        crate::capture::Rect {
            x: rect.left,
            y: rect.top,
            width: (rect.right - rect.left) as u32,
            height: (rect.bottom - rect.top) as u32,
        }
    }
    pub fn new(width: u32, height: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || unsafe {
            let module = GetModuleHandleW(None).unwrap();
            let class = w!("SnapframeRecordingValidation");
            unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
                DefWindowProcW(hwnd, msg, w, l)
            }
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                class,
                w!("Snapframe recording validation"),
                WS_POPUP,
                0,
                0,
                width as i32,
                height as i32,
                None,
                None,
                Some(module.into()),
                None,
            )
            .unwrap();
            let (device, context) = windows_capture::d3d11::create_d3d_device().unwrap();
            let context1: ID3D11DeviceContext1 = context.cast().unwrap();
            let factory: IDXGIFactory2 = CreateDXGIFactory1().unwrap();
            let chain = factory
                .CreateSwapChainForHwnd(
                    &device,
                    hwnd,
                    &DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        ..Default::default()
                    },
                    None,
                    None,
                )
                .unwrap();
            let buffer: ID3D11Texture2D = chain.GetBuffer(0).unwrap();
            let mut target = None;
            device
                .CreateRenderTargetView(&buffer, None, Some(&mut target))
                .unwrap();
            let target = target.unwrap();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let mut counter = 0u32;
            let mut ready = false;
            while !done.load(Ordering::Acquire) {
                let color = (counter % 100) as f32 / 500.0;
                context.ClearRenderTargetView(
                    &target,
                    &[0.5 + color, 0.25 + color, 0.125 + color, 1.0],
                );
                let x = ((counter * 8) % width) as i32;
                context1.ClearView(
                    &target,
                    &[1.0; 4],
                    Some(&[RECT {
                        left: x,
                        top: 0,
                        right: (x + 12).min(width as i32),
                        bottom: height as i32,
                    }]),
                );
                context1.ClearView(
                    &target,
                    &[0.0, 0.0, 0.0, 1.0],
                    Some(&[RECT {
                        left: 0,
                        top: 0,
                        right: 256,
                        bottom: 16,
                    }]),
                );
                for bit in 0..16 {
                    if counter & (1 << bit) != 0 {
                        context1.ClearView(
                            &target,
                            &[1.0; 4],
                            Some(&[RECT {
                                left: bit * 16,
                                top: 0,
                                right: bit * 16 + 16,
                                bottom: 16,
                            }]),
                        );
                    }
                }
                chain.Present(1, DXGI_PRESENT(0)).ok().unwrap();
                if !ready {
                    let _ = tx.send(hwnd.0 as isize);
                    ready = true;
                }
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                counter = counter.wrapping_add(1);
            }
            drop(target);
            drop(buffer);
            drop(chain);
            let _ = DestroyWindow(hwnd);
        });
        Self {
            hwnd: rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Animation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
