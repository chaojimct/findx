# Installer transaction integration candidate (not publish-ready)

The official ISS now invokes the Windows transaction helper for preparation, verification, commit and deinitialization rollback. Unconditional image-name force-kill and post-copy service uninstall/reinstall were removed. Runtime restoration is exact-service and prior-state based.

A running legacy 2.5.2 writer has no final-save acknowledgement. This implementation requests normal stop but REFUSES replacement and restores the old running policy rather than equating exit0/checksum with latest data saved. An already-stopped legacy writer or no-service path can proceed after verified backup. This is a deliberate compatibility limitation, not an engine fix.

Required before production readiness: package-level native CI matrix, complete privileged backup/journal ACL and directory-security/ADS handling, registry/Run/PATH transactional coverage (PATH modification deliberately not executed by this candidate), concurrent/reentry recovery, actual current-GUI payload startup health, and full release gate evidence review. Do not flip publish_ready or publish this candidate. Test builds are fixture-named, CI-namespace bounded, never update assets.
