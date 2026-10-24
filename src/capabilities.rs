use serde_json::{Value,json};
pub fn value()->Value {json!({
    "schema_version":1,"ok":true,"status":"completed","command":"capabilities",
    "program_version":env!("CARGO_PKG_VERSION"),"output_versions":[1],"transport":"local_cli",
    "commands":[
        {"name":"query overview|summary|growth|folder|largest","access":"read","permissions":"read existing history; no elevation, scan or deletion"},
        {"name":"cleanup list|get|runs|status","access":"read","permissions":"read existing history"},
        {"name":"cleanup save|preview|pause|delete","access":"write","permissions":"local data write; preview reads only specified scope"},
        {"name":"cleanup enable","access":"write","permissions":"fresh preview revision and explicit continuous-rule authorization"},
        {"name":"cleanup execute","access":"delete","permissions":"fresh plan and explicit irreversible-deletion authorization; never elevates"},
        {"name":"cleanup cancel","access":"write","permissions":"stop subsequent files; completed deletion is not rolled back"},
        {"name":"scan","access":"scan","permissions":"NTFS volume access; scans only"},
        {"name":"discard-scan","access":"write","permissions":"discard unfinished scan work; completed history retained"},
        {"name":"scan-auto","access":"scan_and_delete","permissions":"existing login lifecycle; enabled rules only"}
    ],
    "preview":{"expires_seconds":900,"candidate_limit":10000,"candidate_json_bytes_limit":8388608,"traversal_entry_limit":200000,"recursion_depth_limit":128,"stored_plan_limit":32,"method":"permanent_delete","execution":"only revalidate previewed identities; never add newly appeared files"},
    "retention":{"item_limit_per_run":200,"item_days":30,"run_limit":1000,"run_days":365,"idempotency":"retained run summaries only; expired plans cannot be replayed"},
    "exit_codes":{"success":0,"failure":1,"partial_or_unverified":2,"cancelled":3,"conflict_or_interrupted":4},
    "authorization":"Command access, plan IDs, revisions and acknowledgement flags do not establish user authorization"
})}
pub fn help() {
    println!("VolumeTrail {}\n\nquery overview|summary|growth|folder|largest --drive C [--json] [--limit 50 --offset 0] [--from ID --to ID] [--path PATH]\ncapabilities --json\ncleanup list|get|save|preview|enable|pause|delete|execute|runs|status|cancel [--json]\n  save --input UTF8_JSON_FILE_OR_- (default disabled; include id/revision to edit)\n  preview --id RULE_ID (or --ids ID,ID)\n  enable --id ID --revision N --plan ID --confirm-automatic-rule\n  pause|delete --id ID --revision N\n  execute --plan ID --request UNIQUE_ID --confirm-permanent-delete\n  status|cancel --id RUN_ID\n  runs [--drive C --limit 50 --offset 0]\nscan C | scan-auto | status C | stop C | discard-scan C\n  scan continues saved work; stop saves progress; discard-scan removes unfinished work.\n\nDeletion is permanent. Preview and obtain explicit user authorization before execute or enable.\nSee docs/automation.md for output, safety and retention contracts.",env!("CARGO_PKG_VERSION"));
}
