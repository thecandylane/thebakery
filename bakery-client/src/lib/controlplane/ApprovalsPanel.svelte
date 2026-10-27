<script lang="ts">
  import ToolRulesPanel from "./ToolRulesPanel.svelte";

  // Settings → Approvals: tool rules only. What an agent may do without
  // asking is its own mode (Manual / Plan / Edit automatically / Auto), set
  // per agent in its composer — there is no node-wide write gate.
</script>

<div class="set-panel" data-testid="approvals-panel">
  <p class="set-lead">
    Each agent's mode — Manual, Plan, Edit automatically, or Auto — is set in its own composer. Tool rules below
    can still forbid a kind of action for every agent.
  </p>
  <ToolRulesPanel />
</div>
