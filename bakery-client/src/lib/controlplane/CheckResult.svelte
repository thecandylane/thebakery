<script lang="ts">
  import type { MachineCheck } from "$lib/node";
  import "$lib/styles/machine-access.css";
  import { tilde } from "./machineAccessModel";

  // What a machine-access check found, run inside a real agent sandbox:
  // whether each boundary held, and which tools work.

  let { result, home = "" }: { result: MachineCheck; home?: string } = $props();
</script>

<div class="ma">
  <div class="card cols" data-testid="machine-check">
    <div>
      <div class="grp">Guards</div>
      {#each result.guards as g (g.what)}
        <div class="row"><span class="who">{g.what}</span><span class="held" class:no={!g.held}>{g.result}</span></div>
      {/each}
    </div>
    <div>
      <div class="grp">Tools</div>
      {#each result.tools as t (t.name)}
        <div class="row">
          <span class="var">{t.name}</span>
          <span class="path" title={t.works ? `${t.path}\n${t.detail}` : t.detail}>{t.works ? tilde(t.path, home) : "—"}</span>
          <span class="held" class:no={!t.works}>{t.works ? "✓" : "✗"}</span>
        </div>
      {/each}
    </div>
  </div>
</div>
