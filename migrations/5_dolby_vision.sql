-- Dolby Vision profile captured at probe time (NULL = not Dolby Vision).
-- Its presence makes the analysis skip the file (SkipReason::DolbyVision): a
-- normal re-encode strips the DV RPU and degrades playback, so DV is left
-- untouched until proper handling exists. The column also lets the affected
-- population be counted directly, independent of workflow state.
ALTER TABLE media_files ADD COLUMN dv_profile SMALLINT;
