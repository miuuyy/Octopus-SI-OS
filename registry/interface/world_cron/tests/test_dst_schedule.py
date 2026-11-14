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

    def fired(self, cron, start, hours, late=timedelta()):
        clock = [start.replace(tzinfo=NEW_YORK).astimezone(timezone.utc)]
        end = clock[0] + timedelta(hours=hours)
        fired = []

        def sleep(seconds):
            clock[0] += timedelta(seconds=seconds) + late
            if clock[0] > end:
                raise InterruptedError

        with patch.object(self.module, "now", lambda schedule: clock[0].astimezone(NEW_YORK)), patch.object(self.module.time, "sleep", sleep), patch.object(self.module, "emit_time_artifact", lambda minute, *rest: fired.append(minute.astimezone(timezone.utc).strftime("%m-%d %H:%MZ"))), patch.object(self.module, "load_last_fired_minute", lambda: None), patch.object(self.module, "commit_last_fired_minute", lambda key: None):
            with self.assertRaises(InterruptedError):
                self.module.run_cron({"cron": cron, "fire_on_start": False, "timezone": NEW_YORK}, "")
        return fired

    def test_schedules_follow_cron_rules_across_both_transitions(self):
        for cron, start, hours, expected in [
            ("*/30 * * * *", datetime(2026, 11, 1, 0, 50), 2.5, ["11-01 05:00Z", "11-01 05:30Z", "11-01 06:00Z", "11-01 06:30Z", "11-01 07:00Z"]),
            ("30 1 * * *", datetime(2026, 10, 31, 12, 0), 48, ["11-01 05:30Z", "11-02 06:30Z"]),
            ("30 2 * * *", datetime(2026, 3, 7, 12, 0), 48, ["03-08 07:00Z", "03-09 06:30Z"]),
            ("0 * * * *", datetime(2026, 3, 8, 0, 30), 3, ["03-08 06:00Z", "03-08 07:00Z", "03-08 08:00Z"]),
        ]:
            with self.subTest(cron=cron, start=start):
                self.assertEqual(self.fired(cron, start, hours), expected)

    def test_a_late_wake_fires_the_due_minute_once(self):
        self.assertEqual(
            self.fired("*/30 * * * *", datetime(2026, 6, 1, 0, 50), 1.6, late=timedelta(minutes=2, seconds=5)),
            ["06-01 05:00Z", "06-01 05:30Z", "06-01 06:00Z"],
        )
        self.assertEqual(
            self.fired("* * * * *", datetime(2026, 6, 1, 0, 50), 0.2, late=timedelta(minutes=3)),
            ["06-01 04:54Z", "06-01 04:58Z", "06-01 05:02Z"],
        )

if __name__ == "__main__":
    unittest.main()
