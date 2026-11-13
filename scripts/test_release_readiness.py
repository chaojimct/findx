import unittest
from check_release import validate
class ReleaseSafety(unittest.TestCase):
    def test_all_version_sources_are_consistent(self):
        self.assertEqual(validate(expected='2.5.0'),('2.5.0',[]))
    def test_wrong_tag_is_rejected(self):
        self.assertTrue(validate(expected='2.4.10')[1])
    def test_unapproved_upgrade_cannot_publish(self):
        errors=validate(require_ready=True)[1]
        self.assertTrue(any('publish_ready=false' in e for e in errors))
        self.assertTrue(any('partial-metadata-periodic-durability' in e for e in errors))
if __name__=='__main__': unittest.main()
