"""Owned public-fixture real IO scan + real-window frame series; no live endpoint or OS input injection."""
import argparse,ctypes as C,ctypes.wintypes as W,json,os,pathlib,subprocess,time,threading,shutil,re
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument("--capture",action="store_true",help="Try same-provider, owned-window primary-monitor screen frames; never changes sessions or focus.");args=parser.parse_args()
from PIL import ImageGrab,Image,ImageOps,ImageDraw
BASE=pathlib.Path.cwd();OUT=BASE/'target/native-usage-t100';OUT.mkdir(exist_ok=True);RUN=OUT/('fixture-'+str(int(time.time())));RUN.mkdir();ROOT=RUN/'public-data';ROOT.mkdir();ENDPOINT='unused';wait=threading.Event().wait
# 384 directories / 3072 small public files, never user folders.
for top,ext,size in [('Code','rs',4096),('Documents','pdf',16384),('Videos','mp4',65536),('Cache','bin',8192),('Pictures','png',24576),('Toolchains','dll',32768),('Workspaces','ts',12288),('Downloads','zip',20480)]:
    for i in range(48):
        d=ROOT/top/f'project-{i:02}';d.mkdir(parents=True)
        for j in range(8):
            with (d/f'public-{j}.{ext}').open('wb')as f:f.truncate(size*(1+(i%3)))
GUI=OUT/'findx2-gui-native.exe';assert GUI.is_file(), 'Build/copy the separate t100 executable first'
K=C.windll.kernel32;U=C.windll.user32;D=C.windll.dwmapi;P=C.windll.psapi
try:U.SetProcessDpiAwarenessContext(C.c_void_p(-4))
except Exception:pass
K.OpenProcess.restype=W.HANDLE;K.CloseHandle.argtypes=[W.HANDLE]
class PMC(C.Structure):_fields_=[('cb',W.DWORD),('faults',W.DWORD)]+[(n,C.c_size_t)for n in['peak_rss','rss','peak_paged','paged','peak_nonpaged','nonpaged','pagefile','peak_pagefile','private']]
P.GetProcessMemoryInfo.argtypes=[W.HANDLE,C.POINTER(PMC),W.DWORD]
U.GetWindowThreadProcessId.argtypes=[W.HWND,C.POINTER(W.DWORD)];U.GetWindowRect.argtypes=[W.HWND,C.POINTER(W.RECT)];U.GetWindowTextW.argtypes=[W.HWND,W.LPWSTR,C.c_int];U.IsWindowVisible.argtypes=[W.HWND];U.SetWindowPos.argtypes=[W.HWND,W.HWND,C.c_int,C.c_int,C.c_int,C.c_int,W.UINT];U.PostMessageW.argtypes=[W.HWND,W.UINT,W.WPARAM,W.LPARAM];U.WindowFromPoint.argtypes=[W.POINT];U.WindowFromPoint.restype=W.HWND;U.GetAncestor.argtypes=[W.HWND,W.UINT];U.GetAncestor.restype=W.HWND;D.DwmGetWindowAttribute.argtypes=[W.HWND,W.DWORD,C.c_void_p,W.DWORD]
CB=C.WINFUNCTYPE(W.BOOL,W.HWND,W.LPARAM)
def window(pid):
    found=[]
    @CB
    def cb(h,_):
        v=W.DWORD();U.GetWindowThreadProcessId(h,C.byref(v))
        if v.value==pid and U.IsWindowVisible(h):found.append(h)
        return True
    U.EnumWindows(cb,0);return found[0]if found else None
def bounds(h):
    r=W.RECT();assert D.DwmGetWindowAttribute(h,9,C.byref(r),C.sizeof(r))==0
    return r
def size_window(h,width):
    o=W.RECT();U.GetWindowRect(h,C.byref(o));v=bounds(h);dw=(o.right-o.left)-(v.right-v.left);dh=(o.bottom-o.top)-(v.bottom-v.top);U.SetWindowPos(h,None,max(0,(U.GetSystemMetrics(0)-width)//2),max(0,(U.GetSystemMetrics(1)-670)//2),width+dw,670+dh,0x0014)
capture_errors=[]
def capture(h):
    if not args.capture or capture_errors:return None
    r=bounds(h)
    for x in [.05,.5,.95]:
        for y in [.05,.5,.95]:
            q=U.WindowFromPoint(W.POINT(int(r.left+x*(r.right-r.left)),int(r.top+y*(r.bottom-r.top))))
            if U.GetAncestor(q,2)!=h:return None
    try:return ImageGrab.grab(bbox=(r.left,r.top,r.right,r.bottom),all_screens=False)
    except OSError as e:capture_errors.append(str(e));return None
def memory(pid):
    h=K.OpenProcess(0x410,False,pid);m=PMC();m.cb=C.sizeof(m)
    try:return {'rss':m.rss,'peak_rss':m.peak_rss,'private':m.private}if P.GetProcessMemoryInfo(h,C.byref(m),m.cb)else{}
    finally:K.CloseHandle(h)
def run(theme,width,cancel=False,reduce=False):
    data=RUN/f'{theme}-{width}-{reduce}';data.mkdir();prefs=data/'FindX-native-dev';prefs.mkdir();(prefs/'prefs.json').write_text(json.dumps({'theme':theme,'usageReduceMotion':reduce,'diagEnabled':False,'previewOpen':False,'sidebarOpen':True}));command=data/'commands';command.write_text('');logpath=data/'gui.log';log=logpath.open('w',encoding='utf8');env=os.environ.copy();env.update(LOCALAPPDATA=str(data),FINDX_NATIVE_USAGE_OPEN=str(ROOT),FINDX_NATIVE_WINDOW_SIZE=f'{width}x670',FINDX_NATIVE_FIXTURE_LOCK='1',FINDX_NATIVE_SCAN_FIXTURE_DELAY_MS='400',FINDX_NATIVE_USAGE_TRACE='1',FINDX_NATIVE_USAGE_FIXTURE_COMMANDS=str(command));env.pop('FINDX_NATIVE_USAGE_INDEXED',None)
    p=subprocess.Popen([str(GUI),'--mock'],env=env,stdout=log,stderr=subprocess.STDOUT);start=time.monotonic();h=None;frames=[];samples=[];events=[];occluded=0
    schedule=[(1.8,'drill'),(3.0,'zoom'),(4.0,'up'),(5.2,'reset'),(6.0,'drill'),(7.0,'up')]+([(8.0,'cancel')]if cancel else[(8.3,'up')])
    try:
        while time.monotonic()-start<20:
            assert p.poll()is None,'owned GUI exited unexpectedly'
            h=window(p.pid)
            if h:break
            wait(.1)
        assert h,'owned fixture window missing';size_window(h,width);start=time.monotonic();at=0
        duration=12 if cancel else 48
        while time.monotonic()-start<duration:
            t=time.monotonic()-start
            while at<len(schedule)and t>=schedule[at][0]:
                with command.open('a')as f:f.write(schedule[at][1]+'\n')
                events.append({'at':t,'command':schedule[at][1]});at+=1
            ready=t>=1.5 and re.search(r'usage_preview revision=[1-9]\d* .*paint_count=[1-9]\d*',logpath.read_text(encoding='utf8',errors='replace')) is not None
            im=capture(h) if ready else None
            if im is not None:
                filename=data/f'frame-{len(frames):04}.png';im.save(filename);frames.append({'seconds':round(t,3),'path':str(filename)})
            else:occluded+=1
            samples.append({'seconds':round(t,3),**memory(p.pid)});wait(.14)
    finally:
        if h:U.PostMessageW(h,0x0010,0,0)
        p.wait(timeout=12);log.close()
    text=logpath.read_text(encoding='utf8',errors='replace');measure=[tuple(map(int,m))for m in re.findall(r'usage_preview revision=(\d+) builds=(\d+) layout_us=(\d+)',text)]
    selected=frames[::max(1,len(frames)//12)][:12];sheet=Image.new('RGB',(4*384,3*276),'#ddd');draw=ImageDraw.Draw(sheet)
    for n,item in enumerate(selected):
        im=Image.open(item['path']).convert('RGB');im.thumbnail((384,250));x=(n%4)*384;y=(n//4)*276;sheet.paste(im,(x,y+22));draw.text((x+5,y+3),f"{item['seconds']:.2f}s",fill='black')
    sheetpath=data/'contact.png' if frames else None
    if sheetpath:sheet.save(sheetpath)
    if frames:
        ims=[Image.open(x['path']).convert('RGB').resize((width,670))for x in frames[::2]];ims[0].save(data/'sequence.gif',save_all=True,append_images=ims[1:],duration=300,loop=0)
        for when in [2,5,9,14,20,40,47]:
            f=min(frames,key=lambda x:abs(x['seconds']-when));shutil.copyfile(f['path'],data/f'{when}s.png')
    return {'theme':theme,'width':width,'reduce':reduce,'cancel':cancel,'pid':p.pid,'exit':p.returncode,'frames':frames,'not_captured_samples':occluded,'capture_errors':list(capture_errors),'events':events,'memory':samples,'layout_samples':measure,'log':str(logpath),'contact':str(sheetpath) if sheetpath else None,'capture_requested':args.capture,'visual_verified':False,'scan_finished':bool(re.search(r'usage_scan .*partial=false',text)),'scan_partial':bool(re.search(r'usage_scan .*partial=true',text))}
report={'fixture_root':str(ROOT),'real_files':3072,'real_dirs':393,'protected_live_pid':56408,'runs':[]}
try:
    report['runs'].append(run('light',1024));report['runs'].append(run('dark',900,cancel=True));report['runs'].append(run('light',900,cancel=True,reduce=True))
finally:(RUN/'dynamic-report.json').write_text(json.dumps(report,indent=2),encoding='utf8')
assert all(x['exit']==0 and (x['scan_partial'] if x['cancel'] else x['scan_finished']) for x in report['runs']), 'owned fixture did not reach expected completed/partial state'
print('Report:',RUN/'dynamic-report.json')
print(json.dumps({**report,'runs':[{k:v for k,v in x.items()if k not in['frames','memory']}for x in report['runs']]},indent=2))
