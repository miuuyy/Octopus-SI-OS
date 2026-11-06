from datetime import datetime, timedelta, timezone
import unittest
from unittest.mock import patch
from zoneinfo import ZoneInfo

import test_one_shot

NEW_YORK = ZoneInfo("America/New_York")


class DaylightSavingSchedule(unittest.TestCase):
    reload = test_one_shot.OneShotContract.reload

    def setUp(self):
        test_one_shot.OneShotContract.setUp(self)

    def fired(self, cron, start, hours):
        clock = [start.replace(tzinfo=NEW_YORK).astimezone(timezone.utc)]
        end = clock[0] + timedelta(hours=hours)
        fired = []

        def sleep(seconds):
            clock[0] += timedelta(seconds=seconds)
            if clock[0] > end:
                raise InterruptedError

        with patch.object(self.module, "now", lambda schedule: clock[0].astimezone(NEW_YORK)), patch.object(self.module.time, "sleep", sleep), patch.object(self.module, "emit_time_artifact", lambda minute, *rest: fired.append(minute.astimezone(timezone.utc).strftime("%m-%d %H:%MZ"))), patch.object(self.module, "load_last_fired_minute", lambda: None), patch.object(self.module, "commit_last_fired_minute", lambda key: None):
            with self.assertRaises(InterruptedError):
                self.module.run_cron({"cron": cron, "fire_on_start": False, "timezone": NEW_YORK}, "")
        return fired

    def test_fall_back_keeps_wildcard_schedules_running_through_the_repeated_hour(self):
        self.assertEqual(self.fired("0 * * * *", datetime(2026, 11, 1, 0, 30), 4), ["11-01 05:00Z", "11-01 06:00Z", "11-01 07:00Z", "11-01 08:00Z"])
        self.assertEqual(self.fired("*/30 * * * *", datetime(2026, 11, 1, 0, 50), 2.5), ["11-01 05:00Z", "11-01 05:30Z", "11-01 06:00Z", "11-01 06:30Z", "11-01 07:00Z"])

    def test_fall_back_runs_a_fixed_time_in_the_repeated_hour_once(self):
        self.assertEqual(self.fired("30 1 * * *", datetime(2026, 10, 31, 12, 0), 48), ["11-01 05:30Z", "11-02 06:30Z"])

    def test_spring_forward_runs_a_skipped_fixed_time_when_the_clock_jumps(self):
        self.assertEqual(self.fired("30 2 * * *", datetime(2026, 3, 7, 12, 0), 48), ["03-08 07:00Z", "03-09 06:30Z"])

    def test_spring_forward_does_not_backfill_wildcard_schedules(self):
        self.assertEqual(self.fired("0 * * * *", datetime(2026, 3, 8, 0, 30), 3), ["03-08 06:00Z", "03-08 07:00Z", "03-08 08:00Z"])


if __name__ == "__main__":
    unittest.main()
