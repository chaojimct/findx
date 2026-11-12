"""Owned fixture process integration; no production endpoint, service controls or cache access."""
import ctypes as C, ctypes.wintypes as W, json, os, pathlib, subprocess, time, threading
from PIL import ImageGrab
BASE=pathlib.Path.cwd(); OUT=BASE/'target'/'native-usage-t97';OUT.mkdir(exist_ok=True)
RUN=OUT/('fixture-'+str(int(time.time())));RUN.mkdir();ROOT=RUN/'public-data';ROOT.mkdir()
for folder in ['Documents','Media','Source','Cache']:(ROOT/folder).mkdir()
for folder,name,size in [('Documents','guide.pdf',3*1024*1024),('Documents','notes.txt',1024*1024),('Media','public-demo.mp4',18*1024*1024),('Media','image.png',2*1024*1024),('Source','main.rs',128*1024),('Source','model.json',256*1024),('Cache','sample.tmp',8*1024*1024)]:
    with (ROOT/folder/name).open('wb') as f:f.truncate(size)
BUILD=BASE/'target'/'native-t97-build'/'debug'
SVC=OUT/'findx2-service.exe';GUI=OUT/'findx2-gui-native.exe'
import shutil
shutil.copy2(BUILD/'findx2-service.exe',SVC);shutil.copy2(BUILD/'findx2-gui-native.exe',GUI)
PIPE='findx-t97-fixture-'+str(os.getpid());ENDPOINT='\\\\.\\pipe\\'+PIPE
EVENTS=RUN/'events.jsonl';EVENTS.write_text('');STOP=RUN/'stop';INDEX=RUN/'index.bin'
K=C.windll.kernel32;U=C.windll.user32;P=C.windll.psapi
try:U.SetProcessDpiAwarenessContext(C.c_void_p(-4))
except Exception:pass
wait=threading.Event().wait
class PMC(C.Structure):
    _fields_=[('cb',W.DWORD),('faults',W.DWORD)]+[(n,C.c_size_t) for n in ['peak_rss','rss','peak_paged','paged','peak_nonpaged','nonpaged','pagefile','peak_pagefile','private']]
K.CloseHandle.argtypes=[W.HANDLE];K.OpenProcess.restype=W.HANDLE;P.GetProcessMemoryInfo.argtypes=[W.HANDLE,C.POINTER(PMC),W.DWORD]
def mem(pid):
    h=K.OpenProcess(0x410,False,pid)
    try:
        p=PMC();p.cb=C.sizeof(p)
        if h and P.GetProcessMemoryInfo(h,C.byref(p),p.cb):return {'rss':p.rss,'peak_rss':p.peak_rss,'private':p.private}
        return {}
    finally:
        if h:K.CloseHandle(h)
def request(req):
    if not K.WaitNamedPipeW(W.LPCWSTR(ENDPOINT),3000):raise RuntimeError('fixture pipe unavailable')
    with open(ENDPOINT,'r+b',buffering=0) as f:
        f.write((json.dumps(req)+'\n').encode());data=bytearray()
        while not data.endswith(b'\n'):
            b=f.read(1)
            if not b:raise RuntimeError('fixture pipe closed')
            data.extend(b)
            if len(data)>1024*1024:raise RuntimeError('response exceeded byte budget')
        return json.loads(data)
def page(path=ROOT,expected=None,after=None):
    r=request({'type':'usage_children','path':str(path),'expected':expected,'after':after,'max_bytes':4096})
    assert r['type']=='usage_children_result',r
    return r['page']
U.GetWindowThreadProcessId.argtypes=[W.HWND,C.POINTER(W.DWORD)]
U.GetWindowRect.argtypes=[W.HWND,C.POINTER(W.RECT)]
U.GetWindowTextW.argtypes=[W.HWND,W.LPWSTR,C.c_int]
U.SetWindowPos.argtypes=[W.HWND,W.HWND,C.c_int,C.c_int,C.c_int,C.c_int,W.UINT]
U.PostMessageW.argtypes=[W.HWND,W.UINT,W.WPARAM,W.LPARAM]
U.WindowFromPoint.argtypes=[W.POINT];U.WindowFromPoint.restype=W.HWND
U.GetAncestor.argtypes=[W.HWND,W.UINT];U.GetAncestor.restype=W.HWND
CALLBACK=C.WINFUNCTYPE(W.BOOL,W.HWND,W.LPARAM)
def find_window(pid):
    found=[]
    @CALLBACK
    def cb(h,_):
        p=W.DWORD();U.GetWindowThreadProcessId(h,C.byref(p))
        if p.value==pid and U.IsWindowVisible(h):
            title=C.create_unicode_buffer(256);U.GetWindowTextW(h,title,256)
            if 'FindX Native' in title.value:found.append(h)
        return True
    U.EnumWindows(cb,0);return found[0] if found else None
D=C.windll.dwmapi
D.DwmGetWindowAttribute.argtypes=[W.HWND,W.DWORD,C.c_void_p,W.DWORD]
def capture(pid,name):
    h=find_window(pid);assert h,'owned fixture GUI window not found'
    sw=U.GetSystemMetrics(0);sh=U.GetSystemMetrics(1)
    U.SetWindowPos(h,None,max(0,(sw-1024)//2),max(0,(sh-670)//2),1024,670,0x0014)
    wait(1)
    outer=W.RECT();U.GetWindowRect(h,C.byref(outer))
    r=W.RECT()
    if D.DwmGetWindowAttribute(h,9,C.byref(r),C.sizeof(r)) != 0:raise RuntimeError('cannot obtain visible fixture frame safely')
    # Exclude invisible resize margins (which contain pixels from other apps).
    extra_w=(outer.right-outer.left)-(r.right-r.left);extra_h=(outer.bottom-outer.top)-(r.bottom-r.top)
    U.SetWindowPos(h,None,max(0,(sw-1024)//2),max(0,(sh-670)//2),1024+extra_w,670+extra_h,0x0014)
    wait(.3)
    if D.DwmGetWindowAttribute(h,9,C.byref(r),C.sizeof(r)) != 0:raise RuntimeError('visible fixture frame unavailable')
    # Refuse to capture foreign foreground content; never raise/focus/inject input.
    for fx in [.08,.5,.92]:
        for fy in [.08,.5,.92]:
            at=U.WindowFromPoint(W.POINT(int(r.left+fx*(r.right-r.left)),int(r.top+fy*(r.bottom-r.top))))
            if U.GetAncestor(at,2)!=h:return {'captured':False,'reason':'owned fixture window is occluded; no foreground manipulation attempted'}
    im=ImageGrab.grab(bbox=(r.left,r.top,r.right,r.bottom),all_screens=True);im.save(OUT/name)
    return {'captured':True,'path':str(OUT/name),'size':im.size,'dpi':U.GetDpiForWindow(h) if hasattr(U,'GetDpiForWindow') else None}
def close_gui(p):
    if p is None:return
    h=find_window(p.pid)
    if h:U.PostMessageW(h,0x0010,0,0)
    try:p.wait(timeout=10)
    except subprocess.TimeoutExpired:raise RuntimeError('owned GUI did not accept normal WM_CLOSE; left running, no kill used')
report={'run':str(RUN),'root':str(ROOT),'pipe':PIPE,'production_touched':False,'samples':[]};gui=None;svc=None
slog=(RUN/'service.log').open('w',encoding='utf8');glog=(RUN/'gui.log').open('w',encoding='utf8')
try:
    start=time.monotonic()
    svc=subprocess.Popen([str(SVC),'--index',str(INDEX),'--pipe',PIPE,'--no-watch','--no-backfill','--no-everything-ipc','--usage-memory-mib','32','--fixture-root',str(ROOT),'--fixture-events',str(EVENTS),'--shutdown-file',str(STOP)],stdout=slog,stderr=subprocess.STDOUT)
    while time.monotonic()-start<20:
        if svc.poll() is not None:raise RuntimeError('fixture service exited: '+str(svc.returncode))
        try:
            cap=request({'type':'usage_capabilities'})
            if cap.get('logical_tree')==1:
                try:page();break
                except AssertionError as e:
                    if 'index loading; usage initializing' not in str(e):raise
        except (OSError,RuntimeError):pass
        wait(.1)
    initial=page();report['initial']=initial;report['startup_to_page_ms']=int((time.monotonic()-start)*1000)
    assert initial['root']['logical']==sum(p.stat().st_size for p in ROOT.rglob('*') if p.is_file())
    assert request({'type':'status'})['type']=='status_result'
    assert request({'type':'search','query':'guide','pinyin':False,'limit':10,'offset':0})['type']=='search_result'
    assert request({'type':'usage_children','path':str(ROOT/'..'),'expected':None,'after':None,'max_bytes':4096})['type']=='error'
    env=os.environ.copy();env['LOCALAPPDATA']=str(RUN/'gui-data');env['FINDX_NATIVE_USAGE_OPEN']=str(ROOT);env['FINDX_NATIVE_USAGE_INDEXED']='1';env['FINDX_NATIVE_WINDOW_SIZE']='1024x670';env['FINDX_NATIVE_FIXTURE_LOCK']='1'
    gui=subprocess.Popen([str(GUI),'--pipe',PIPE],env=env,stdout=glog,stderr=subprocess.STDOUT)
    deadline=time.monotonic()+25
    while not find_window(gui.pid) and time.monotonic()<deadline:
        if gui.poll() is not None:raise RuntimeError('fixture GUI exited')
        wait(.1)
    for _ in range(12):report['samples'].append({'elapsed_ms':int((time.monotonic()-start)*1000),'service':mem(svc.pid),'gui':mem(gui.pid)});wait(.25)
    report['screenshot_initial']=capture(gui.pid,'t97-index-logical-initial-1024.png')
    manifest=json.loads(INDEX.with_suffix('.fixture-manifest.json').read_text());ids={pathlib.Path(x['path']).relative_to(ROOT).as_posix():x['id'] for x in manifest};rootid=ids['.'];left=ids['Documents'];right=ids['Source'];media=ids['Media'];fid=ids['Documents/guide.pdf']
    operations=[{'op':'create','file_id':10000,'parent_id':left,'name':'added.txt','size':12345}, {'op':'size','file_id':10000,'size':54321}, {'op':'size','file_id':10000,'size':100}, {'op':'rename','file_id':10000,'parent_id':right,'name':'renamed.txt'}, {'op':'rename','file_id':media,'parent_id':right,'name':'MovedMedia'}, {'op':'delete','file_id':fid}, {'op':'delete','file_id':10000}]
    base=initial['root']['logical'];expected=base
    report['events']=[]
    for i,op in enumerate(operations):
        begin=time.monotonic();before=page()
        if op['op']=='create':expected+=12345
        elif i==1:expected+=54321-12345
        elif i==2:expected+=100-54321
        elif i==5:expected-=3*1024*1024
        elif i==6:expected-=100
        with EVENTS.open('a',encoding='utf8') as f:f.write(json.dumps(op)+'\n')
        deadline=time.monotonic()+5
        while time.monotonic()<deadline:
            current=page()
            if current['generation']>before['generation'] and current['root']['logical']==expected:break
            wait(.025)
        assert current['root']['logical']==expected,(op,current)
        stale=request({'type':'usage_children','path':str(ROOT),'expected':[before['epoch'],before['generation']],'after':None,'max_bytes':4096});assert stale['type']=='error'
        report['events'].append({'operation':op,'logical':expected,'generation':current['generation'],'commit_to_page_ms':int((time.monotonic()-begin)*1000)})
    child=page(ROOT/'Source');assert any(n['name']=='MovedMedia' for n in child['children']);report['drill_source']=child
    for _ in range(16):report['samples'].append({'elapsed_ms':int((time.monotonic()-start)*1000),'service':mem(svc.pid),'gui':mem(gui.pid)});wait(.25)
    report['final']=page();report['screenshot_after_events']=capture(gui.pid,'t97-index-logical-updated-1024.png')
    report['service_pid']=svc.pid;report['gui_pid']=gui.pid
finally:
    try:close_gui(gui);report['gui_exit_code']=None if gui is None else gui.returncode
    finally:
        if svc is not None:
            STOP.write_text('stop own fixture only');svc.wait(timeout=12);report['service_exit_code']=svc.returncode
        slog.close();glog.close();(OUT/'integration-report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf8')
print(json.dumps(report,ensure_ascii=False,indent=2))
