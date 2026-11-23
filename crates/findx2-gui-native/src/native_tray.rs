//! Per-window Windows notification icon. No global hotkey, service or registry changes.
//! Subclassing preserves GPUI's window procedure. All HWND calls stay on the UI thread.
#![cfg(windows)]
use std::{cell::Cell, mem::size_of, sync::mpsc::{self, Receiver, SyncSender}};
use windows::{core::{w, PCWSTR}, Win32::{Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM}, UI::{Shell::*, WindowsAndMessaging::*}}};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event { Show, Exit }
struct State { callback:u32, taskbar:u32, data:NOTIFYICONDATAW, tx:SyncSender<Event>, ready:Cell<bool> }
pub struct Tray { hwnd:HWND, state:Box<State>, rx:Receiver<Event> }
const SUBCLASS:usize=0x46495854;
impl Tray {
    pub fn new(window:&gpui_kit::Window, title:&str)->Option<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let RawWindowHandle::Win32(h)=HasWindowHandle::window_handle(window).ok()?.as_raw() else{return None};
        Self::for_hwnd(HWND(h.hwnd.get() as *mut _), title)
    }
    fn for_hwnd(hwnd:HWND,title:&str)->Option<Self> {
        let (tx,rx)=mpsc::sync_channel(8);
        unsafe {
            let callback=RegisterWindowMessageW(w!("FindX.Native.Tray.v1"));
            let taskbar=RegisterWindowMessageW(w!("TaskbarCreated"));
            if callback==0 || taskbar==0 {return None;}
            let mut data=NOTIFYICONDATAW::default();
            data.cbSize=size_of::<NOTIFYICONDATAW>() as u32;data.hWnd=hwnd;data.uID=1;
            data.uFlags=NIF_MESSAGE|NIF_ICON|NIF_TIP;data.uCallbackMessage=callback;
            let module=windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok();
            data.hIcon=module.and_then(|m|LoadIconW(Some(HINSTANCE(m.0)),PCWSTR(1usize as *const u16)).ok())
                .or_else(||LoadIconW(None,IDI_APPLICATION).ok())?;
            for (out,ch) in data.szTip.iter_mut().take(127).zip(title.encode_utf16()){*out=ch;}
            let mut state=Box::new(State{callback,taskbar,data,tx,ready:Cell::new(false)});
            if !SetWindowSubclass(hwnd,Some(callback_proc),SUBCLASS,(&mut *state) as *mut State as usize).as_bool(){return None;}
            state.ready.set(Shell_NotifyIconW(NIM_ADD,&state.data).as_bool());
            if !state.ready.get() {let _=RemoveWindowSubclass(hwnd,Some(callback_proc),SUBCLASS);return None;}
            Some(Self{hwnd,state,rx})
        }
    }
    pub fn ready(&self)->bool { self.state.ready.get() }
    pub fn event(&self)->Option<Event>{self.rx.try_recv().ok()}
    pub fn hide(&self){unsafe{let _=ShowWindow(self.hwnd,SW_HIDE);}}
    pub fn show(&self){unsafe{let _=ShowWindow(self.hwnd,if IsIconic(self.hwnd).as_bool(){SW_RESTORE}else{SW_SHOW});let _=SetForegroundWindow(self.hwnd);}}
}
impl Drop for Tray {
    fn drop(&mut self){unsafe{let _=Shell_NotifyIconW(NIM_DELETE,&self.state.data);let _=RemoveWindowSubclass(self.hwnd,Some(callback_proc),SUBCLASS);}}
}
unsafe extern "system" fn callback_proc(hwnd:HWND,msg:u32,wp:WPARAM,lp:LPARAM,_:usize,reference:usize)->LRESULT {
    // The Box is owned by the window entity and the subclass is removed before it drops.
    let s=unsafe{&*(reference as *const State)};
    if msg==s.taskbar {
        s.ready.set(unsafe{Shell_NotifyIconW(NIM_ADD,&s.data)}.as_bool());
        // Failed recreation must not strand a hidden window without its icon.
        if !s.ready.get() {unsafe{let _=ShowWindow(hwnd,SW_RESTORE);}}
    } else if msg==s.callback {
        match lp.0 as u32 {
            WM_LBUTTONUP|WM_LBUTTONDBLCLK => {let _=s.tx.try_send(Event::Show);}
            WM_RBUTTONUP|WM_CONTEXTMENU => unsafe {
                if let Ok(menu)=CreatePopupMenu(){
                    let _=AppendMenuW(menu,MF_STRING,1,w!("显示 FindX"));
                    let _=AppendMenuW(menu,MF_STRING,2,w!("退出 FindX（不停止索引服务）"));
                    let mut p=POINT::default();let _=GetCursorPos(&mut p);let _=SetForegroundWindow(hwnd);
                    let id=TrackPopupMenu(menu,TPM_RETURNCMD|TPM_NONOTIFY|TPM_RIGHTBUTTON,p.x,p.y,None,hwnd,None).0;
                    let _=DestroyMenu(menu);let _=PostMessageW(Some(hwnd),WM_NULL,WPARAM(0),LPARAM(0));
                    match id {1=>{let _=s.tx.try_send(Event::Show);},2=>{let _=s.tx.try_send(Event::Exit);},_=>{}}
                }
            },
            _=>{}
        }
        return LRESULT(0);
    } else if msg==WM_NCDESTROY {
        s.ready.set(false);unsafe{let _=Shell_NotifyIconW(NIM_DELETE,&s.data);let _=RemoveWindowSubclass(hwnd,Some(callback_proc),SUBCLASS);}
    }
    unsafe{DefSubclassProc(hwnd,msg,wp,lp)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn isolated_native_icon_registration_events_recreation_and_cleanup() {
        unsafe {
            let hwnd=CreateWindowExW(WINDOW_EX_STYLE::default(),w!("STATIC"),w!("FindX t164 hidden tray fixture"),WS_OVERLAPPED,0,0,1,1,None,None,None,None).unwrap();
            assert!(!IsWindowVisible(hwnd).as_bool());
            let tray=Tray::for_hwnd(hwnd,"FindX t164 isolated tray fixture").expect("interactive Windows shell needed for tray smoke");
            assert!(tray.ready());
            SendMessageW(hwnd,tray.state.callback,Some(WPARAM(1)),Some(LPARAM(WM_LBUTTONUP as isize)));
            assert_eq!(tray.event(),Some(Event::Show));assert_eq!(tray.event(),None);
            // Mimic only THIS fixture icon being lost, not an Explorer restart.
            assert!(Shell_NotifyIconW(NIM_DELETE,&tray.state.data).as_bool());
            SendMessageW(hwnd,tray.state.taskbar,None,None);
            assert!(tray.ready());
            for _ in 0..100 {SendMessageW(hwnd,tray.state.callback,Some(WPARAM(1)),Some(LPARAM(WM_LBUTTONUP as isize)));}
            let mut queued=0;while tray.event().is_some(){queued+=1;}assert_eq!(queued,8);
            assert!(!IsWindowVisible(hwnd).as_bool());
            drop(tray);assert!(IsWindow(Some(hwnd)).as_bool());
            let _=DestroyWindow(hwnd);
        }
    }
}
