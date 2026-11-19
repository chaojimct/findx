//! Real runtime work ownership. Internal prepared observations remain NON-ACK.
use findx2_ipc::{StopToken, DurableStopReceipt};
use findx2_core::{checkpoint_fence::FreezeRequest, SearchEngine};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    marker::PhantomData,
    path::PathBuf,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant},
};
thread_local! {static DEPTH:RefCell<HashMap<usize,usize>>=RefCell::new(HashMap::new());}
#[derive(Default)]
struct Work {
    closed: bool,
    stopping: bool,
    active: usize,
    failed: Option<String>,
    terminal_error: Option<String>,
    sources: HashMap<&'static str, usize>,
    outcome_epoch: u64,
    errors: HashMap<&'static str, (u64, String)>,
}
pub(crate) struct State {
    key: usize,
    control: Mutex<Option<mpsc::Sender<Command>>>,
    index_identity: String,
    work: Mutex<Work>,
    changed: Condvar,
}
fn table() -> &'static Mutex<HashMap<usize, Weak<State>>> {
    static T: OnceLock<Mutex<HashMap<usize, Weak<State>>>> = OnceLock::new();
    T.get_or_init(Default::default)
}
fn lookup(e: &SearchEngine) -> Option<Arc<State>> {
    table()
        .lock()
        .unwrap()
        .get(&(e as *const _ as usize))
        .and_then(Weak::upgrade)
}
fn nested(key: usize) -> bool {
    DEPTH.with(|d| d.borrow().get(&key).copied().unwrap_or(0) > 0)
}
pub(crate) struct Reservation {
    state: Arc<State>,
    source: &'static str,
    admitted_epoch: u64,
    failure: Option<String>,
    required: bool,
    completed: bool,
    recover: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut w = self.state.work.lock().unwrap();
        // Outcome publication and the last active decrement share ONE mutex.
        // A waiter can never see active==0 without seeing this operation's error.
        let failure = self.failure.take().or_else(|| {
            (self.required && !self.completed).then(|| {
                format!(
                    "{} exited without completing its real operation",
                    self.source
                )
            })
        });
        if let Some(error) = failure {
            match w.outcome_epoch.checked_add(1) {
                Some(v) => w.outcome_epoch = v,
                None => w.failed = Some("runtime outcome epoch exhausted".into()),
            }
            let epoch = w.outcome_epoch;
            w.errors.insert(self.source, (epoch, error));
        } else if self.recover
            && w.errors
                .get(self.source)
                .is_some_and(|(epoch, _)| *epoch <= self.admitted_epoch)
        {
            w.errors.remove(self.source);
        }
        w.active -= 1;
        if let Some(n) = w.sources.get_mut(self.source) {
            *n -= 1;
        }
        if std::thread::panicking() {
            w.failed = Some(format!("{} panicked; RAM retained, no ACK", self.source));
        }
        self.state.changed.notify_all();
    }
}
impl Reservation {
    pub(crate) fn require_result(mut self) -> Self {
        self.required = true;
        self
    }
    pub(crate) fn success(&mut self) {
        self.completed = true;
    }
    pub(crate) fn error(&mut self, error: String) {
        self.completed = true;
        self.failure = Some(error);
    }
    pub(crate) fn activate(self) -> Activity {
        let key = self.state.key;
        DEPTH.with(|d| *d.borrow_mut().entry(key).or_default() += 1);
        Activity {
            key: Some(key),
            _owned: Some(self),
            _not_send: PhantomData,
        }
    }
}
pub(crate) struct Activity {
    key: Option<usize>,
    _owned: Option<Reservation>,
    _not_send: PhantomData<Rc<()>>,
}
impl Activity {
    pub(crate) fn require_result(mut self) -> Self {
        if let Some(p) = &mut self._owned {
            p.required = true;
        }
        self
    }
    pub(crate) fn success(&mut self) {
        if let Some(p) = &mut self._owned {
            p.success();
        }
    }
    pub(crate) fn error(&mut self, error: String) {
        if let Some(p) = &mut self._owned {
            p.error(error);
        }
    }
    pub(crate) fn finish<T, E: std::fmt::Display>(mut self, result: Result<T, E>) -> Result<T, E> {
        match &result {
            Ok(_) => self.success(),
            Err(e) => self.error(e.to_string()),
        };
        result
    }
    pub(crate) fn finish_persistence(
        mut self,
        result: anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        if matches!(&result, Ok(true)) {
            if let Some(p) = &mut self._owned {
                p.recover = true;
            }
        }
        self.finish(result)
    }
}
pub(crate) fn error_optional(activity: &mut Option<Activity>, error: String) {
    if let Some(a) = activity {
        a.error(error);
    }
}
pub(crate) fn success_optional(activity: &mut Option<Activity>) {
    if let Some(a) = activity {
        a.success();
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        if let Some(k) = self.key {
            DEPTH.with(|d| {
                let mut d = d.borrow_mut();
                let n = d.get_mut(&k).unwrap();
                *n -= 1;
                if *n == 0 {
                    d.remove(&k);
                }
            });
        }
    }
}
fn empty() -> Activity {
    Activity {
        key: None,
        _owned: None,
        _not_send: PhantomData,
    }
}
impl State {
    fn reserve(self: &Arc<Self>, source: &'static str) -> Result<Reservation, String> {
        let parent = nested(self.key);
        let mut w = self.work.lock().unwrap();
        while w.closed && !parent {
            if w.stopping {
                return Err("runtime stopping; producer admission revoked".into());
            }
            w = self.changed.wait(w).unwrap();
        }
        w.active += 1;
        *w.sources.entry(source).or_default() += 1;
        Ok(Reservation {
            state: self.clone(),
            source,
            admitted_epoch: w.outcome_epoch,
            failure: None,
            required: false,
            completed: false,
            recover: false,
        })
    }
    fn reopen(&self) {
        let mut w = self.work.lock().unwrap();
        if !w.stopping && w.failed.is_none() {
            w.closed = false;
        }
        self.changed.notify_all();
    }
    fn stop_ingress(&self) {
        let mut w = self.work.lock().unwrap();
        w.closed = true;
        w.stopping = true;
        self.changed.notify_all();
    }
    fn close_drain(
        &self,
        until: Instant,
        cancel: &AtomicBool,
        stop: Option<&AtomicBool>,
    ) -> Result<u64, String> {
        let mut w = self.work.lock().unwrap();
        w.closed = true;
        self.changed.notify_all();
        loop {
            if let Some(e) = &w.failed {
                return Err(e.clone());
            }
            if cancel.load(Ordering::Acquire)
                || stop.is_some_and(|s| s.load(Ordering::Acquire))
                || Instant::now() >= until
            {
                return Err(format!(
                    "runtime drain cancelled/timeout/stop; active={} sources={:?}; no ACK",
                    w.active, w.sources
                ));
            }
            if w.active == 0 {
                if !w.stopping && !w.errors.is_empty() {
                    return Err(format!(
                        "runtime source errors retained; no ACK: {:?}",
                        w.errors
                    ));
                }
                return Ok(w.outcome_epoch);
            }
            w = self
                .changed
                .wait_timeout(w, Duration::from_millis(5))
                .unwrap()
                .0;
        }
    }
    fn publish_if_current<T>(
        &self,
        epoch: u64,
        valid: impl FnOnce() -> bool,
        publish: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let w = self.work.lock().unwrap();
        if !w.closed
            || w.stopping
            || w.active != 0
            || w.failed.is_some()
            || !w.errors.is_empty()
            || w.outcome_epoch != epoch
            || !valid()
        {
            return Err("runtime changed/failed before observation publication; no ACK".into());
        }
        Ok(publish())
    }
}
pub(crate) fn reserve(
    e: &SearchEngine,
    source: &'static str,
) -> Result<Option<Reservation>, String> {
    lookup(e)
        .map(|s| s.reserve(source).map(Some))
        .unwrap_or(Ok(None))
}
pub(crate) fn enter(e: &SearchEngine, source: &'static str) -> Result<Activity, String> {
    if let Some(s) = lookup(e) {
        return s.reserve(source).map(Reservation::activate);
    }
    Ok(empty())
}
pub(crate) fn error_snapshot(e: &SearchEngine) -> Option<String> {
    lookup(e).and_then(|s| {
        let w = s.work.lock().unwrap();
        let mut errors: Vec<_> = w
            .errors
            .iter()
            .map(|(source, (_, error))| format!("{source}: {error}"))
            .collect();
        if let Some(e) = &w.failed { errors.push(e.clone()); }
        if let Some(e) = &w.terminal_error { errors.push(e.clone()); }
        (!errors.is_empty()).then(|| errors.join("; "))
    })
}

enum Command {
    Commit { token: StopToken, reply: mpsc::Sender<Result<(DurableStopReceipt,String),String>> },
    Cancel { token: StopToken, reply: mpsc::Sender<Result<(),String>> },
    PreparedStop { reply: mpsc::Sender<Result<Option<DurableStopReceipt>,String>> },
    Prepare {
        request: FreezeRequest,
        until: Instant,
        cancel: Arc<AtomicBool>,
        reply: mpsc::Sender<Result<serde_json::Value, String>>,
    },
    Abort {
        instance: String,
        nonce: String,
        generation: u64,
        reply: mpsc::Sender<Result<(), String>>,
    },
    DrainStop {
        until: Instant,
        reply: mpsc::Sender<Result<Reservation, String>>,
    },
    Shutdown,
}
pub(crate) struct Owner {
    state: Arc<State>,
    tx: mpsc::Sender<Command>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Owner {
    pub(crate) fn attach(
        engine: Arc<SearchEngine>,
        index: PathBuf,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let key = Arc::as_ptr(&engine) as usize;
        let identity = index.parent().ok_or("index parent missing")?.canonicalize().map_err(|e|e.to_string())?.join(index.file_name().ok_or("index filename missing")?);
        let state = Arc::new(State {
            key,
            control: Mutex::new(None),
            index_identity: identity.to_string_lossy().into_owned(),
            work: Mutex::new(Work::default()),
            changed: Condvar::new(),
        });
        {
            let mut t = table().lock().unwrap();
            if t.get(&key).and_then(Weak::upgrade).is_some() {
                return Err("duplicate lifecycle actor".into());
            }
            t.insert(key, Arc::downgrade(&state));
        }
        let (tx, rx) = mpsc::channel();
        *state.control.lock().unwrap()=Some(tx.clone());
        let s = state.clone();
        let thread=std::thread::Builder::new().name("findx2-prepare-actor".into()).spawn(move||{
   let mut prepared_token:Option<StopToken>=None;let mut committed:Option<(DurableStopReceipt,String)>=None;
   let mut frame=None;let mut binding:Option<(String,String,Instant,u64,Arc<AtomicBool>)>=None;let mut consumed=HashSet::new();
   loop{
    if stop.load(Ordering::Acquire){if committed.is_none(){frame.take();binding.take();prepared_token.take();}s.stop_ingress();}
    if committed.is_none()&&binding.is_some()&&{let w=s.work.lock().unwrap();w.failed.is_some()||!w.errors.is_empty()}{frame.take();binding.take();s.reopen();}
    if committed.is_none()&&binding.as_ref().is_some_and(|b|Instant::now()>=b.2||b.4.load(Ordering::Acquire)){frame.take();binding.take();s.reopen();}
    match rx.recv_timeout(Duration::from_millis(20)){
     Ok(Command::Prepare{request,until,cancel,reply})=>{
      if binding.is_some()||stop.load(Ordering::Acquire){let _=reply.send(Err("already prepared or stopping; no ACK".into()));continue;}
      let instance=request.expected_instance.clone();let nonce=request.nonce.clone();
      if consumed.len()>=4096||!consumed.insert(nonce.clone()){let _=reply.send(Err("request nonce replay or bounded ledger exhausted; no ACK".into()));continue;}
      let result=s.close_drain(until,&cancel,Some(&stop)).and_then(|epoch|engine.freeze_checkpoint(&index,request,until,&cancel).map(|f|(epoch,f)));
      match result{
       Ok((epoch,f)) if !stop.load(Ordering::Acquire)&&!cancel.load(Ordering::Acquire)&&Instant::now()<until=>{
        let observation=serde_json::json!({"upgrade_ack":false,"managed_work_drained":true,"outcome_epoch":epoch,"process_stopped":false,"core":f.evidence()});
        let generation=observation["core"]["published_generation"].as_u64().expect("serialized u64 generation");let core=&observation["core"];
        prepared_token=Some(StopToken{protocol_version:2,engine_instance:instance.clone(),service_pid:std::process::id(),request_nonce:nonce.clone(),index_identity:core["index_identity"].as_str().unwrap().into(),mutation_sequence:core["mutation_sequence"].as_u64().unwrap(),published_generation:generation,outcome_epoch:epoch});
        let token=cancel.clone();frame=Some(f);binding=Some((instance,nonce,until,generation,cancel));
        match s.publish_if_current(epoch,||!stop.load(Ordering::Acquire)&&!token.load(Ordering::Acquire)&&Instant::now()<until,||reply.send(Ok(observation))){
         Ok(Ok(()))=>{},
         Ok(Err(_))=>{frame.take();binding.take();s.reopen();},
         Err(e)=>{frame.take();binding.take();s.reopen();let _=reply.send(Err(e));}
        }
       }
       Ok((_,f))=>{drop(f);s.reopen();let _=reply.send(Err("late/cancelled/stop completion; no ACK".into()));}
       Err(e)=>{s.reopen();let _=reply.send(Err(e));}
      }
     }
     Ok(Command::Commit{token,reply})=>{
      if let Some((receipt,path))=&committed {let result=if receipt.token==token{validate_committed(&s,&index,receipt,path,frame.is_some()).map(|_|(receipt.clone(),path.clone()))}else{Err("different committed request".into())};let _=reply.send(result);continue;}
      let valid=prepared_token.as_ref()==Some(&token)&&frame.as_ref().is_some_and(|f|serde_json::to_value(f.evidence()).ok().is_some_and(|v|v["metadata_ready"]==true))&&binding.as_ref().is_some_and(|b|Instant::now()<b.2&&!b.4.load(Ordering::Acquire))&&!stop.load(Ordering::Acquire);
      if !valid {let _=reply.send(Err("wrong/expired instance nonce index sequence generation or epoch; no receipt".into()));continue;}
      let until=binding.as_ref().unwrap().2;
      let result=s.publish_if_current(token.outcome_epoch,||Instant::now()<until&&!stop.load(Ordering::Acquire),||write_terminal_receipt(&index,&token,until)).and_then(|v|v);
      match result {Ok(v)=>{committed=Some(v.clone());let _=reply.send(Ok(v));},Err(e)=>{frame.take();binding.take();prepared_token.take();s.reopen();let _=reply.send(Err(e));}}
     }
     Ok(Command::Cancel{token,reply})=>{
      if committed.is_some(){let _=reply.send(Err("already durably committed; sealed safe state, request normal SCM STOP".into()));}
      else if prepared_token.as_ref()==Some(&token)&&frame.is_some(){frame.take();binding.take();prepared_token.take();s.reopen();let _=reply.send(Ok(()));}
      else{let _=reply.send(Err("no current exact preparation; no cancellation".into()));}
     }
     Ok(Command::PreparedStop{reply})=>{
      let result=if let Some((receipt,path))=&committed {validate_committed(&s,&index,receipt,path,frame.is_some()).map(|_|Some(receipt.clone()))}else{Ok(None)};
      let _=reply.send(result);
     }
     Ok(Command::Abort{instance,nonce,generation,reply})=>{
      if committed.is_none()&&binding.as_ref().is_some_and(|b|b.0==instance&&b.1==nonce&&b.3==generation){frame.take();binding.take();s.reopen();let _=reply.send(Ok(()));}else{let _=reply.send(Err("stale instance/nonce or expired prepare".into()));}
     }
     Ok(Command::DrainStop{until,reply})=>{
      if committed.is_some(){let _=reply.send(Err("terminal checkpoint retained; use prepared_stop, never reopen and resave".into()));continue;}
      frame.take();binding.take();s.stop_ingress();
      let result=s.close_drain(until,&AtomicBool::new(false),None).map(|_|{let mut w=s.work.lock().unwrap();w.active+=1;*w.sources.entry("stop-final-save").or_default()+=1;Reservation{state:s.clone(),source:"stop-final-save",admitted_epoch:w.outcome_epoch,failure:None,required:false,completed:false,recover:false}});
      let _=reply.send(result);
     }
     Ok(Command::Shutdown)|Err(mpsc::RecvTimeoutError::Disconnected)=>{frame.take();s.stop_ingress();break;}
     Err(mpsc::RecvTimeoutError::Timeout)=>{}
    }
   }
  }).map_err(|e|e.to_string())?;
        Ok(Self {
            state,
            tx,
            thread: Some(thread),
        })
    }
    pub(crate) fn retain_terminal_failure(&self,error:String){
        let mut w=self.state.work.lock().unwrap();
        if w.failed.is_none(){w.outcome_epoch=w.outcome_epoch.saturating_add(1);w.failed=Some(error);}
        w.closed=true;w.stopping=true;self.state.changed.notify_all();
    }
    pub(crate) fn prepared_stop(&self)->Result<Option<DurableStopReceipt>,String>{
        let(tx,rx)=mpsc::channel();self.tx.send(Command::PreparedStop{reply:tx}).map_err(|e|e.to_string())?;rx.recv().map_err(|e|e.to_string())?
    }
    pub(crate) fn retained_errors(&self) -> Vec<String> {
        let w = self.state.work.lock().unwrap();
        let mut errors: Vec<_> = w
            .errors
            .iter()
            .map(|(source, (_, error))| format!("{source}: {error}"))
            .collect();
        if let Some(e) = &w.failed { errors.push(e.clone()); }
        if let Some(e) = &w.terminal_error { errors.push(e.clone()); }
        errors
    }
    pub(crate) fn drain_stop(&self, until: Instant) -> Result<Activity, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::DrainStop { until, reply: tx })
            .map_err(|e| e.to_string())?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map(Reservation::activate)
    }
    #[cfg(test)]
    fn prepare(
        &self,
        r: FreezeRequest,
        until: Instant,
        cancel: Arc<AtomicBool>,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::Prepare {
                request: r,
                until,
                cancel,
                reply: tx,
            })
            .unwrap();
        rx.recv().unwrap()
    }
    #[cfg(test)]
    fn abort(&self, instance: String, nonce: String, generation: u64) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(Command::Abort {
                instance,
                nonce,
                generation,
                reply: tx,
            })
            .unwrap();
        rx.recv().unwrap()
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        table().lock().unwrap().remove(&self.state.key);
    }
}

// Revalidate EVERY committed retry and STOP: neither an old in-memory success nor
// a lost response may hide later producer errors or actual receipt/pointer IO failure.
fn validate_committed(state:&State,index:&std::path::Path,receipt:&DurableStopReceipt,path:&str,has_frame:bool)->Result<(),String>{
    let mut w=state.work.lock().unwrap();
    let result=(||{
        if !has_frame||!w.closed||w.active!=0||!w.errors.is_empty()||w.failed.is_some()||w.outcome_epoch!=receipt.token.outcome_epoch{return Err("committed frame/outcome failed; RAM retained sealed, no ACK or successful STOP".to_string());}
        let meta=std::fs::metadata(path).map_err(|e|format!("terminal receipt metadata: {e}"))?;
        if meta.len()>16384{return Err("terminal receipt exceeds bound".into());}
        let bytes=std::fs::read(path).map_err(|e|format!("terminal receipt read: {e}"))?;
        let disk:DurableStopReceipt=serde_json::from_slice(&bytes).map_err(|e|format!("terminal receipt decode: {e}"))?;
        if disk!=*receipt{return Err("terminal durable receipt no longer equals committed token".into());}
        let pointer=findx2_core::persist_tx::read_published(&findx2_core::V6Layout::for_index(index)).map_err(|e|format!("terminal published pointer: {e}"))?.ok_or("terminal published pointer missing")?;
        if pointer.manifest.generation!=receipt.token.published_generation{return Err("terminal actual disk generation changed; no ACK".into());}
        Ok(())
    })();
    // This diagnostic is NOT a producer mutation epoch. Only a successful actual
    // receipt+pointer read clears its own transient IO failure; never source errors.
    w.terminal_error=result.as_ref().err().cloned();result
}

// The real save_full writer has already synced/published the exact bound generation,
// and its FrozenCheckpoint still owns namespace AND publication exclusion here.
fn write_terminal_receipt(index:&std::path::Path, token:&StopToken, until:Instant)->Result<(DurableStopReceipt,String),String>{
    use std::io::Write;
    let path=findx2_core::V6Layout::for_index(index).dir.join(format!("stop-{}-{}.json",token.engine_instance,token.request_nonce));
    let tmp=path.with_extension("tmp");
    let receipt=DurableStopReceipt{token:token.clone(),state:"saved_stop_pending".into(),process_stopped:false};
    let result=(||{
        if Instant::now()>=until{return Err("commit deadline before receipt IO".to_string());}
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e|format!("receipt create: {e}"))?;
        file.write_all(&serde_json::to_vec(&receipt).map_err(|e|e.to_string())?).map_err(|e|format!("receipt write: {e}"))?;
        file.sync_all().map_err(|e|format!("receipt sync: {e}"))?;drop(file);
        if Instant::now()>=until{return Err("commit deadline after receipt IO; no ACK".into());}
        findx2_core::persist_tx::atomic_rename_replace(&tmp,&path).map_err(|e|format!("receipt publish: {e}"))?;
        if Instant::now()>=until{let _=std::fs::remove_file(&path);return Err("late receipt publish; no ACK".into());}
        Ok((receipt,path.to_string_lossy().into_owned()))
    })();if result.is_err(){let _=std::fs::remove_file(tmp);}result
}

/// Only the Windows pipe transport with a verified elevated client calls this.
pub(crate) fn management(e:&SearchEngine, req:findx2_ipc::IpcRequest)->findx2_ipc::IpcResponse{
    use findx2_ipc::{IpcRequest as Q,IpcResponse as R};
    let result=(||->Result<R,String>{
        let s=lookup(e).ok_or("runtime lifecycle unavailable")?;
        let tx=s.control.lock().unwrap().clone().ok_or("actor unavailable")?;
        match req{
            Q::CheckpointIdentity{protocol_version:2}=>Ok(R::CheckpointIdentityResult{protocol_version:2,engine_instance:e.checkpoint_instance()?,service_pid:std::process::id(),index_identity:s.index_identity.clone()}),
            Q::PrepareStop{protocol_version:2,expected_instance,request_nonce,index_identity,timeout_ms}=>{
                if index_identity!=s.index_identity||!(50..=120000).contains(&timeout_ms){return Err("wrong index or timeout bound".into());}
                if crate::watch_health::watch_error_summary().is_some()||e.backfill_error_snapshot().is_some(){return Err("native watch/backfill unhealthy; no prepare".into());}
                let until=Instant::now()+Duration::from_millis(timeout_ms);let cancel=Arc::new(AtomicBool::new(false));let(a,b)=mpsc::channel();
                tx.send(Command::Prepare{request:FreezeRequest{protocol_version:1,expected_instance,nonce:request_nonce},until,cancel:cancel.clone(),reply:a}).map_err(|e|e.to_string())?;
                let observed=match b.recv_timeout(Duration::from_millis(timeout_ms)){Ok(v)=>v?,Err(_)=>{cancel.store(true,Ordering::Release);return Err("prepare timeout; RAM retained, no receipt".into())}};
                let c=&observed["core"];
                if c["metadata_ready"]!=true {let(a,_)=mpsc::channel();let _=tx.send(Command::Abort{instance:c["engine_instance"].as_str().unwrap().into(),nonce:c["request_nonce"].as_str().unwrap().into(),generation:c["published_generation"].as_u64().unwrap(),reply:a});return Err("metadata not ready; preparation aborted".into());}
                Ok(R::StopPrepared{token:StopToken{protocol_version:2,engine_instance:c["engine_instance"].as_str().unwrap().into(),service_pid:std::process::id(),request_nonce:c["request_nonce"].as_str().unwrap().into(),index_identity:c["index_identity"].as_str().unwrap().into(),mutation_sequence:c["mutation_sequence"].as_u64().unwrap(),published_generation:c["published_generation"].as_u64().unwrap(),outcome_epoch:observed["outcome_epoch"].as_u64().unwrap()}})
            },
            Q::CommitStop{token}=>{let(a,b)=mpsc::channel();tx.send(Command::Commit{token,reply:a}).map_err(|e|e.to_string())?;let(receipt,receipt_path)=b.recv().map_err(|e|e.to_string())??;Ok(R::StopCommitted{receipt,receipt_path})},
            Q::CancelStop{token}=>{let(a,b)=mpsc::channel();tx.send(Command::Cancel{token,reply:a}).map_err(|e|e.to_string())?;b.recv().map_err(|e|e.to_string())??;Ok(R::StopCancelled)},
            _=>Err("unsupported management protocol; refuse before STOP".into())
        }
    })();result.unwrap_or_else(|message|R::Error{message})
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn fixture(label: &str) -> (PathBuf, Arc<SearchEngine>, Owner, Arc<AtomicBool>) {
        let d = std::env::temp_dir().join(format!(
            "findx-runtime-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(d.join("data")).unwrap();
        std::fs::write(d.join("data/public.txt"), b"public").unwrap();
        let path = d.join("index.bin");
        crate::usage_fixture::create_index(&d.join("data"), &path).unwrap();
        let mut store = findx2_core::load_index_bin(&path).unwrap();
        store.metadata_ready = false;
        store.set_track_mutations(true);
        findx2_core::v6_save_full(&findx2_core::V6Layout::for_index(&path), &store).unwrap();
        let e = Arc::new(SearchEngine::new(store));
        e.enable_usage(16 * 1024 * 1024).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let owner = Owner::attach(e.clone(), path.clone(), stop.clone()).unwrap();
        (path, e, owner, stop)
    }
    fn req(e: &SearchEngine, n: char) -> FreezeRequest {
        FreezeRequest {
            protocol_version: 1,
            expected_instance: e.checkpoint_instance().unwrap(),
            nonce: n.to_string().repeat(32),
        }
    }
    fn generation(v: &serde_json::Value) -> u64 {
        v["core"]["published_generation"].as_u64().unwrap()
    }
    fn until() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }
    fn prepare_async(
        o: &Owner,
        r: FreezeRequest,
    ) -> mpsc::Receiver<Result<serde_json::Value, String>> {
        let (tx, rx) = mpsc::channel();
        o.tx.send(Command::Prepare {
            request: r,
            until: until(),
            cancel: Arc::new(AtomicBool::new(false)),
            reply: tx,
        })
        .unwrap();
        rx
    }
    fn idx(e: &SearchEngine) -> usize {
        e.index_store()
            .entries
            .iter()
            .position(|e| !e.is_dir_entry())
            .unwrap()
    }
    fn size(path: &std::path::Path) -> u64 {
        let s = findx2_core::v6_load(&findx2_core::V6Layout::for_index(path)).unwrap();
        s.entries.iter().find(|e| !e.is_dir_entry()).unwrap().size
    }
    #[test]
    fn runtime_actual_precaptured_persister_drains_before_frozen_observation() {
        let (p, e, o, _) = fixture("captured");
        e.patch_entry_metadata(idx(&e), 456, 0, 0).unwrap();
        let (tx, rx) = mpsc::channel();
        let (go, gone) = mpsc::channel();
        let worker = e.clone();
        let path = p.clone();
        let t = std::thread::spawn(move || {
            crate::run::persist_index_with(&worker, &path, |layout, batch| {
                tx.send(()).unwrap();
                gone.recv_timeout(Duration::from_secs(2)).unwrap();
                findx2_core::v6_try_commit(layout, batch).map_err(anyhow::Error::from)
            })
            .unwrap()
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let prepared = prepare_async(&o, req(&e, 'a'));
        assert!(prepared.recv_timeout(Duration::from_millis(20)).is_err());
        go.send(()).unwrap();
        t.join().unwrap();
        let v = prepared
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(v["upgrade_ack"], false);
        assert_eq!(v["managed_work_drained"], true);
        assert_eq!(size(&p), 456);
        assert!(o
            .abort(
                e.checkpoint_instance().unwrap(),
                "a".repeat(32),
                generation(&v) + 1
            )
            .is_err());
        assert!(o
            .abort(
                e.checkpoint_instance().unwrap(),
                "stale".repeat(8),
                generation(&v)
            )
            .is_err());
        o.abort(
            e.checkpoint_instance().unwrap(),
            "a".repeat(32),
            generation(&v),
        )
        .unwrap();
        assert!(o
            .prepare(req(&e, 'a'), until(), Arc::new(AtomicBool::new(false)))
            .is_err());
        assert!(
            e.has_pending_persistence(),
            "abort must force full repair, not reapply snapshot deltas"
        );
        assert!(crate::run::persist_index(&e, &p).unwrap());
        assert_eq!(size(&p), 456);
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_real_metadata_pipeline_and_queued_child_finish_while_closing() {
        let (p, e, o, _) = fixture("metadata");
        let (tx, rx) = mpsc::channel();
        let (go, gone) = mpsc::channel();
        let worker = e.clone();
        let epoch = e.metadata_epoch();
        let t = std::thread::spawn(move || {
            crate::metadata_pipeline::ticket_pass(
                &worker,
                epoch,
                &AtomicBool::new(false),
                crate::metadata_pipeline::Limits::default(),
                |_, items| {
                    tx.send(()).unwrap();
                    gone.recv_timeout(Duration::from_secs(2)).unwrap();
                    Ok((0..items.len()).map(|i| (i, 789, 0, 0)).collect())
                },
            )
            .unwrap()
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let prepared = prepare_async(&o, req(&e, 'b'));
        assert!(prepared.recv_timeout(Duration::from_millis(20)).is_err());
        go.send(()).unwrap();
        t.join().unwrap();
        let v = prepared
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert_eq!(size(&p), 789);
        o.abort(
            e.checkpoint_instance().unwrap(),
            "b".repeat(32),
            generation(&v),
        )
        .unwrap();
        let job = reserve(&e, "metadata-finalizer").unwrap().unwrap();
        let prepared = prepare_async(&o, req(&e, 'c'));
        assert!(prepared.recv_timeout(Duration::from_millis(20)).is_err());
        let worker = e.clone();
        let path = p.clone();
        let t = std::thread::spawn(move || {
            let _job = job.activate();
            crate::run::persist_index(&worker, &path).unwrap();
        });
        t.join().unwrap();
        let v = prepared
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        o.abort(
            e.checkpoint_instance().unwrap(),
            "c".repeat(32),
            generation(&v),
        )
        .unwrap();
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_stop_revokes_waiters_without_fence_deadlock_and_failed_save_retains_ram() {
        let (p, e, o, stop) = fixture("stop");
        e.patch_entry_metadata(idx(&e), 999, 0, 0).unwrap();
        o.prepare(req(&e, 'd'), until(), Arc::new(AtomicBool::new(false)))
            .unwrap();
        let worker = e.clone();
        let t = std::thread::spawn(move || enter(&worker, "late-callback").is_err());
        stop.store(true, Ordering::Release);
        let root = o.drain_stop(until()).unwrap();
        assert!(t.join().unwrap());
        findx2_core::set_persist_fault(findx2_core::PersistFault::AfterDataSync);
        let result = crate::run::persist_index(&e, &p);
        findx2_core::clear_persist_fault();
        assert!(result.is_err());
        let i = idx(&e);
        assert_eq!(e.index_store().entries[i].size, 999);
        assert!(e.has_pending_persistence());
        assert!(crate::run::persist_index(&e, &p).unwrap());
        assert_eq!(size(&p), 999);
        drop(root);
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_cancel_after_observation_releases_real_fence_without_ack() {
        let (p, e, o, _) = fixture("cancel-observed");
        let cancel = Arc::new(AtomicBool::new(false));
        let v = o.prepare(req(&e, '9'), until(), cancel.clone()).unwrap();
        let (worker, done) = (e.clone(), mpsc::channel());
        let (tx, rx) = done;
        let t = std::thread::spawn(move || {
            let _work = enter(&worker, "late-after-cancel").unwrap();
            worker
                .patch_entry_metadata(idx(&worker), 654, 0, 0)
                .unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(10)).is_err());
        cancel.store(true, Ordering::Release);
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        t.join().unwrap();
        assert!(o
            .abort(
                e.checkpoint_instance().unwrap(),
                "9".repeat(32),
                generation(&v)
            )
            .is_err());
        assert!(crate::run::persist_index(&e, &p).unwrap());
        assert_eq!(size(&p), 654);
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_timeout_cancel_and_instance_mismatch_do_not_stop_or_clear_engine() {
        let (p, e, o, _) = fixture("refuse");
        e.patch_entry_metadata(idx(&e), 321, 0, 0).unwrap();
        let held = enter(&e, "queued-actual-work").unwrap();
        let result = o.prepare(
            req(&e, 'e'),
            Instant::now() + Duration::from_millis(15),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(result.is_err());
        drop(held);
        let i = idx(&e);
        assert_eq!(e.index_store().entries[i].size, 321);
        assert!(o
            .prepare(req(&e, 'f'), until(), Arc::new(AtomicBool::new(true)))
            .is_err());
        let mut wrong = req(&e, 'g');
        wrong.expected_instance = "old-instance".into();
        assert!(o
            .prepare(wrong, until(), Arc::new(AtomicBool::new(false)))
            .is_err());
        let observation = o
            .prepare(req(&e, 'h'), until(), Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(observation["process_stopped"], false);
        o.abort(
            e.checkpoint_instance().unwrap(),
            "h".repeat(32),
            generation(&observation),
        )
        .unwrap();
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn runtime_failed_prepare_real_writer_stages_keep_ram_reopen_and_retry() {
        use findx2_core::PersistFault;
        for (i, fault, expected) in [
            (
                0,
                PersistFault::DiskFull { max_bytes: 0 },
                "disk full (injected quota",
            ),
            (
                1,
                PersistFault::ShortWrite {
                    file: "entries.dat".into(),
                    max_bytes: 0,
                },
                "injected short write",
            ),
            (
                2,
                PersistFault::AccessDenied {
                    file: "entries.dat".into(),
                },
                "injected access failure",
            ),
            (3, PersistFault::AfterDataSync, "after data sync"),
            (4, PersistFault::BeforePublish, "before publish"),
            (5, PersistFault::RenameFail, "rename failure"),
        ] {
            let (p, e, o, _) = fixture(&format!("actor-fault-{i}"));
            e.patch_entry_metadata(idx(&e), 700 + i, 0, 0).unwrap();
            findx2_core::set_persist_fault(fault);
            let result = o.prepare(req(&e, '1'), until(), Arc::new(AtomicBool::new(false)));
            findx2_core::clear_persist_fault();
            assert!(
                matches!(result,Err(ref error) if error.contains(expected)),
                "must reach actual requested writer phase: {result:?}"
            );
            let entry = idx(&e);
            assert_eq!(e.index_store().entries[entry].size, 700 + i);
            assert!(e.has_pending_persistence());
            {
                let _work = enter(&e, "actual-retry-producer").unwrap();
                e.patch_entry_metadata(entry, 800 + i, 0, 0).unwrap();
            }
            let v = o
                .prepare(req(&e, '2'), until(), Arc::new(AtomicBool::new(false)))
                .unwrap();
            assert_eq!(v["upgrade_ack"], false);
            assert_eq!(size(&p), 800 + i);
            o.abort(
                e.checkpoint_instance().unwrap(),
                "2".repeat(32),
                generation(&v),
            )
            .unwrap();
            drop(o);
            std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn runtime_actual_failed_persister_publishes_error_before_last_permit_and_retry_clears_only_own_error(
    ) {
        let (p, e, o, _) = fixture("atomic-result");
        e.patch_entry_metadata(idx(&e), 900, 0, 0).unwrap();
        let (tx, rx) = mpsc::channel();
        let (go, gone) = mpsc::channel();
        let worker = e.clone();
        let path = p.clone();
        let t = std::thread::spawn(move || {
            crate::run::persist_index_with(&worker, &path, |layout, batch| {
                tx.send(()).unwrap();
                gone.recv().unwrap();
                findx2_core::set_persist_fault(findx2_core::PersistFault::AfterDataSync);
                let result = findx2_core::v6_try_commit(layout, batch).map_err(anyhow::Error::from);
                findx2_core::clear_persist_fault();
                result
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let prepared = prepare_async(&o, req(&e, '3'));
        assert!(prepared.recv_timeout(Duration::from_millis(10)).is_err());
        go.send(()).unwrap();
        let error = t.join().unwrap().unwrap_err();
        assert!(format!("{error:#}").contains("after data sync"));
        let refusal = prepared
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert!(
            refusal.contains("after data sync"),
            "must observe actual writer error before active becomes zero: {refusal}"
        );
        assert!(!o.retained_errors().is_empty());
        assert!(e.has_pending_persistence());
        assert!(crate::run::persist_index(&e, &p).unwrap());
        assert!(o.retained_errors().is_empty());
        let v = o
            .prepare(req(&e, '4'), until(), Arc::new(AtomicBool::new(false)))
            .unwrap();
        assert_eq!(size(&p), 900);
        o.abort(
            e.checkpoint_instance().unwrap(),
            "4".repeat(32),
            generation(&v),
        )
        .unwrap();
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_real_metadata_error_stays_visible_after_persist_and_old_instance_is_rejected_after_restart(
    ) {
        let (p, e, o, _) = fixture("metadata-result");
        let error = crate::metadata_pipeline::ticket_pass(
            &e,
            e.metadata_epoch(),
            &AtomicBool::new(false),
            crate::metadata_pipeline::Limits::default(),
            |_, _| Err("actual pipeline fetch failure".into()),
        )
        .unwrap_err();
        assert!(error.contains("actual pipeline fetch failure"));
        let slot = Arc::new(std::sync::RwLock::new(Some(e.clone())));
        assert!(
            matches!(crate::ipc_dispatch::process_request(&slot,findx2_ipc::IpcRequest::UpgradeCapabilities{protocol_version:1}),findx2_ipc::IpcResponse::UpgradeCapabilitiesResult{durable_prepare:false,runtime_error:Some(ref error),..} if error.contains("actual pipeline fetch failure"))
        );
        assert!(o
            .prepare(req(&e, '5'), until(), Arc::new(AtomicBool::new(false)))
            .unwrap_err()
            .contains("actual pipeline fetch failure"));
        e.patch_entry_metadata(idx(&e), 901, 0, 0).unwrap();
        crate::run::persist_index(&e, &p).unwrap();
        assert!(
            !o.retained_errors().is_empty(),
            "unrelated successful disk IO cannot repair source failure"
        );
        let stale = req(&e, '6');
        drop(o);
        let restarted = Arc::new(SearchEngine::new(
            findx2_core::v6_load(&findx2_core::V6Layout::for_index(&p)).unwrap(),
        ));
        restarted.enable_usage(16 * 1024 * 1024).unwrap();
        let owner = Owner::attach(
            restarted.clone(),
            p.clone(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_ne!(
            restarted.checkpoint_instance().unwrap(),
            stale.expected_instance
        );
        assert!(owner
            .prepare(stale, until(), Arc::new(AtomicBool::new(false)))
            .unwrap_err()
            .contains("instance"));
        assert_eq!(size(&p), 901);
        drop(owner);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn runtime_missing_normal_outcome_and_late_error_cannot_publish_clean_observation() {
        let (p, e, o, _) = fixture("late-outcome");
        let permit = enter(&e, "unreported-source").unwrap().require_result();
        let prepared = prepare_async(&o, req(&e, '7'));
        drop(permit);
        assert!(prepared
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err()
            .contains("without completing"));
        let epoch = {
            let mut w = o.state.work.lock().unwrap();
            w.closed = true;
            w.outcome_epoch
        };
        let mut published = false;
        assert!(o
            .state
            .publish_if_current(epoch, || true, || published = true)
            .is_err());
        assert!(!published);
        drop(o);
        std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    fn token(v:&serde_json::Value)->StopToken{
        let c=&v["core"];StopToken{protocol_version:2,engine_instance:c["engine_instance"].as_str().unwrap().into(),service_pid:std::process::id(),request_nonce:c["request_nonce"].as_str().unwrap().into(),index_identity:c["index_identity"].as_str().unwrap().into(),mutation_sequence:c["mutation_sequence"].as_u64().unwrap(),published_generation:c["published_generation"].as_u64().unwrap(),outcome_epoch:v["outcome_epoch"].as_u64().unwrap()}
    }
    fn commit(o:&Owner,t:StopToken)->Result<(DurableStopReceipt,String),String>{let(a,b)=mpsc::channel();o.tx.send(Command::Commit{token:t,reply:a}).unwrap();b.recv_timeout(Duration::from_secs(3)).unwrap()}
    #[test]
    fn terminal_real_writer_generation_and_seals_survive_stop_no_resave(){
        let(p,e,o,stop)=fixture("terminal-success");e.index_store_mut().metadata_ready=true;
        let i=idx(&e);e.patch_entry_metadata(i,981,0,0).unwrap();
        let v=o.prepare(req(&e,'T'),until(),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);
        for field in 0..8{let mut bad=t.clone();match field{0=>bad.protocol_version=1,1=>bad.engine_instance.push('x'),2=>bad.service_pid+=1,3=>bad.request_nonce.push('x'),4=>bad.index_identity.push('x'),5=>bad.mutation_sequence+=1,6=>bad.published_generation+=1,_=>bad.outcome_epoch+=1};assert!(commit(&o,bad).is_err());}
        let(saved,path)=commit(&o,t.clone()).unwrap();assert!(!saved.process_stopped);assert_eq!(saved.state,"saved_stop_pending");assert_eq!(serde_json::from_slice::<DurableStopReceipt>(&std::fs::read(path).unwrap()).unwrap(),saved);
        let(read_tx,read_rx)=mpsc::channel();let worker=e.clone();let blocked=std::thread::spawn(move||{worker.patch_entry_metadata(i,999,0,0).unwrap();read_tx.send(()).unwrap();});
        assert!(read_rx.recv_timeout(Duration::from_millis(30)).is_err());
        stop.store(true,Ordering::Release);assert_eq!(o.prepared_stop().unwrap(),Some(saved.clone()));assert_eq!(commit(&o,t).unwrap().0,saved);assert!(o.drain_stop(until()).is_err());
        assert_eq!(size(&p),981);assert!(read_rx.recv_timeout(Duration::from_millis(30)).is_err(),"STOP reopened actual namespace fence");
        drop(o);blocked.join().unwrap();std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn terminal_real_receipt_io_failure_releases_for_retry_without_clearing_ram(){
        let(p,e,o,_)=fixture("terminal-receipt-failure");e.index_store_mut().metadata_ready=true;let i=idx(&e);e.patch_entry_metadata(i,1234,0,0).unwrap();
        let v=o.prepare(req(&e,'R'),until(),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);
        let path=findx2_core::V6Layout::for_index(&p).dir.join(format!("stop-{}-{}.json",t.engine_instance,t.request_nonce));std::fs::create_dir(path.with_extension("tmp")).unwrap();
        assert!(commit(&o,t.clone()).unwrap_err().contains("receipt create"));assert!(!path.exists());assert!(o.prepared_stop().unwrap().is_none());assert!(commit(&o,t).is_err());
        assert_eq!(e.index_store().entries[i].size,1234);e.patch_entry_metadata(i,4321,0,0).unwrap();
        let v=o.prepare(req(&e,'S'),until(),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);assert!(commit(&o,t).is_ok());assert_eq!(size(&p),4321);drop(o);std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn terminal_metadata_not_ready_never_receipts_even_with_guessed_complete_token(){
        let(p,e,o,_)=fixture("terminal-metadata-incomplete");let v=o.prepare(req(&e,'M'),until(),Arc::new(AtomicBool::new(false))).unwrap();assert!(commit(&o,token(&v)).is_err());drop(o);std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

    #[test]
    fn terminal_lost_reply_and_expired_original_deadline_keep_committed_seal_and_retry_identity(){
        let(p,e,o,stop)=fixture("terminal-lost-reply");e.index_store_mut().metadata_ready=true;
        let v=o.prepare(req(&e,'L'),Instant::now()+Duration::from_millis(300),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);
        let(a,b)=mpsc::channel();drop(b);o.tx.send(Command::Commit{token:t.clone(),reply:a}).unwrap();
        let receipt=o.prepared_stop().unwrap().unwrap();assert_eq!(receipt.token,t);
        std::thread::sleep(Duration::from_millis(330));assert_eq!(commit(&o,t.clone()).unwrap().0,receipt);
        let(a,b)=mpsc::channel();o.tx.send(Command::Cancel{token:t.clone(),reply:a}).unwrap();assert!(b.recv().unwrap().is_err());
        stop.store(true,Ordering::Release);assert_eq!(o.prepared_stop().unwrap(),Some(receipt));drop(o);
        let fresh=Owner::attach(e.clone(),p.clone(),Arc::new(AtomicBool::new(false))).unwrap();assert!(commit(&fresh,t).is_err());drop(fresh);std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn terminal_actual_late_receipt_io_failure_is_not_hidden_and_only_real_repair_allows_retry(){
        let(p,e,o,stop)=fixture("terminal-late-io");e.index_store_mut().metadata_ready=true;let i=idx(&e);e.patch_entry_metadata(i,2468,0,0).unwrap();
        let v=o.prepare(req(&e,'I'),until(),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);let(saved,path)=commit(&o,t.clone()).unwrap();let bytes=std::fs::read(&path).unwrap();std::fs::remove_file(&path).unwrap();
        assert!(commit(&o,t.clone()).unwrap_err().contains("terminal receipt"));assert!(error_snapshot(&e).unwrap().contains("terminal receipt"));stop.store(true,Ordering::Release);assert!(o.prepared_stop().is_err());assert_eq!(size(&p),2468);
        std::fs::write(&path,bytes).unwrap();assert_eq!(o.prepared_stop().unwrap(),Some(saved));assert!(error_snapshot(&e).is_none());assert!(commit(&o,t).is_ok());drop(o);std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }
    #[test]
    fn terminal_late_worker_failure_rejects_old_ack_and_retains_actual_fence(){
        let(p,e,o,stop)=fixture("terminal-late-worker");e.index_store_mut().metadata_ready=true;let v=o.prepare(req(&e,'W'),until(),Arc::new(AtomicBool::new(false))).unwrap();let t=token(&v);commit(&o,t.clone()).unwrap();
        // This is the exact publication invoked by run.rs AFTER an actual worker join failure.
        o.retain_terminal_failure("joined native watch worker returned failure".into());stop.store(true,Ordering::Release);
        assert!(o.prepared_stop().is_err());assert!(commit(&o,t).is_err());assert!(error_snapshot(&e).unwrap().contains("joined native"));assert!(e.try_index_store().is_none(),"failed STOP dropped the authoritative RAM fence");drop(o);std::fs::remove_dir_all(p.parent().unwrap()).unwrap();
    }

}

#[cfg(all(test,windows))]
mod terminal_tests {
    use super::*;
    #[test]
    fn terminal_token_has_no_default_identity_or_generation_and_public_dispatch_refuses(){
        assert!(serde_json::from_str::<StopToken>(r#"{"protocol_version":2}"#).is_err());
        let slot=Arc::new(std::sync::RwLock::new(None));
        assert!(matches!(crate::ipc_dispatch::process_request(&slot,findx2_ipc::IpcRequest::CheckpointIdentity{protocol_version:2}),findx2_ipc::IpcResponse::Error{..}));
    }
}
