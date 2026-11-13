import importlib.util,unittest
from pathlib import Path
spec=importlib.util.spec_from_file_location('policy',Path(__file__).with_name('t119-resource-policy.py'));m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);S=m.Sample;G=m.GIB
class Tests(unittest.TestCase):
 def test_stable(self):
  p=m.Policy()
  for i in range(20):self.assertIsNone(p.observe(S(i*.2,6*G,12*G,G,G)))
 def test_own_resident_initialization_is_not_external_burn(self):
  p=m.Policy();p.observe(S(0,6*G,12*G,int(.1*G),int(.1*G)));self.assertIsNone(p.observe(S(1,int(5.2*G),int(11.2*G),int(.9*G),int(.9*G))))
 def test_fast_external_burn_stops_before_old_floor(self):
  p=m.Policy();p.observe(S(0,6*G,12*G,G,G));r=p.observe(S(1,int(5.5*G),12*G,G,G));self.assertEqual(r['reason'],'external_burn_forecast');self.assertGreater(r['available'],5*G)
 def test_absolute_floor(self):self.assertEqual(m.Policy().observe(S(0,int(3.4*G),12*G))['reason'],'conservative_physical_floor')
 def test_commit_floor(self):self.assertEqual(m.Policy().observe(S(0,6*G,int(2.9*G)))['reason'],'commit_headroom_floor')
 def test_small_fluctuations(self):
  p=m.Policy();p.observe(S(0,6*G,12*G));self.assertIsNone(p.observe(S(1,6*G-80*1024**2,12*G)))
 def test_nonresident_commit_cannot_hide_external_burn(self):
  p=m.Policy();p.observe(S(0,6*G,12*G,G,G));self.assertEqual(p.observe(S(1,int(5.5*G),12*G,int(1.5*G),G))['reason'],'external_burn_forecast')
 def test_latched_stop(self):
  p=m.Policy();r=p.observe(S(0,3*G,12*G));self.assertEqual(p.observe(S(1,10*G,12*G)),r)
 def test_monotonic(self):
  p=m.Policy();p.observe(S(1,6*G,12*G))
  with self.assertRaises(ValueError):p.observe(S(1,6*G,12*G))
 def test_stable_start_gate(self):self.assertTrue(m.startup_gate([S(i,6*G,12*G) for i in range(21)],20)[0])
 def test_stale_and_sparse_start_gate(self):
  self.assertFalse(m.startup_gate([S(0,6*G,12*G),S(1,6*G,12*G)],20)[0]);self.assertFalse(m.startup_gate([S(19,6*G,12*G),S(20,6*G,12*G)],20)[0])
 def test_low_preflight_headroom(self):self.assertFalse(m.startup_gate([S(i,4*G if i==10 else 6*G,12*G) for i in range(21)],20)[0])
if __name__=='__main__':unittest.main(verbosity=2)
