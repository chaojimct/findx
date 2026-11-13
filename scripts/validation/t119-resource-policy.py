"""Conservative owned-process stop requests, not an OS/global memory guarantee.
No process is killed and no privilege, priority or unrelated workload is changed.
"""
from collections import deque
from typing import NamedTuple
GIB=1024**3
class Sample(NamedTuple):
 t:float
 physical:int
 commit_available:int
 private:int=0
 ws:int=0
class Policy:
 def __init__(self,physical_floor=int(3.5*GIB),commit_floor=3*GIB,hard_floor=int(1.5*GIB),grace_seconds=12):
  self.history=deque(maxlen=32);self.reason=None;self.physical_floor=physical_floor;self.commit_floor=commit_floor;self.hard_floor=hard_floor;self.grace=grace_seconds
 def observe(self,s):
  if self.reason:return self.reason
  if self.history and s.t<=self.history[-1].t:raise ValueError('non-monotonic safety sample')
  if s.physical<=self.physical_floor:self.reason={'reason':'conservative_physical_floor','available':s.physical}
  elif s.commit_available<=self.commit_floor:self.reason={'reason':'commit_headroom_floor','available':s.commit_available}
  else:
   rate=0.
   for old in self.history:
    dt=s.t-old.t
    if .5<=dt<=3.0:
     # Only growth which is both private and resident is attributed to our
     # process. Private commit alone must not conceal another physical burn.
     own_growth=max(0,min(s.private-old.private,s.ws-old.ws))
     rate=max(rate,(old.physical-s.physical-own_growth)/dt)
   forecast=s.physical-rate*self.grace
   if rate>=128*1024**2 and forecast<self.hard_floor:self.reason={'reason':'external_burn_forecast','available':s.physical,'estimated_external_bytes_per_second':round(rate),'forecast_after_grace':round(forecast),'grace_seconds':self.grace}
  self.history.append(s)
  return self.reason

def startup_gate(rows,now,stable_seconds=20):
 if not rows:return False,'no system history'
 recent=[s for s in rows if now-stable_seconds<=s.t<=now]
 if not recent or now-recent[-1].t>3:return False,'stale system history'
 if recent[-1].t-recent[0].t<stable_seconds-1:return False,'insufficient stable history'
 if min(s.physical for s in recent)<5*GIB:return False,'less than 5GiB physical during preflight window'
 if min(s.commit_available for s in recent)<8*GIB:return False,'less than 8GiB commit headroom during preflight window'
 return True,'stable physical/commit preflight passed'
