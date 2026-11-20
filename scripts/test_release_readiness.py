import unittest
from check_release import validate
class ReleaseSafety(unittest.TestCase):
    def test_all_version_sources_are_consistent(self):
        self.assertEqual(validate(expected='2.5.8'),('2.5.8',[]))
    def test_wrong_tag_is_rejected(self):
        self.assertTrue(validate(expected='2.4.10')[1])
    def test_unpublished_candidate_stays_closed(self):
        errors=validate(expected='2.5.8', require_ready=True)[1]
        self.assertTrue(any('publish_ready=false' in e for e in errors))
if __name__=='__main__': unittest.main()
