default: run

run:
    cargo run

show-logs:
    jq . logfile.json|bat --language=json
