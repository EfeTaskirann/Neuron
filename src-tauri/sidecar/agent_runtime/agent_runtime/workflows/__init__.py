"""Workflow registry.

Week 2 ships a single hardcoded workflow (`daily-summary`) per Charter
§"Hard constraints" #6. Multi-workflow support lives in Week 3.
"""

from agent_runtime.workflows.daily_summary import (
    DAILY_SUMMARY_ID,
    run_daily_summary,
)

__all__ = ["DAILY_SUMMARY_ID", "run_daily_summary", "WORKFLOWS"]


# Flat dispatch table — keyed by the same `workflowId` the frontend
# passes through `runs:create`. Adding a workflow is one entry here.
WORKFLOWS = {
    DAILY_SUMMARY_ID: run_daily_summary,
}
