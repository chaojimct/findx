import unittest
from check_release import validate
class ReleaseSafety(unittest.TestCase):
    def test_all_version_sources_are_consistent(self):
        self.assertEqual(validate(expected='2.5.4'),('2.5.4',[]))
    def test_wrong_tag_is_rejected(self):
        self.assertTrue(validate(expected='2.4.10')[1])
    def test_fresh_tag_publishes_without_a_seal_plan(self):
        self.assertEqual(validate(expected='2.5.4', require_ready=True)[1], [])
if __name__=='__main__': unittest.main()
