rw_run() {
    way_in_lock=/tmp/$(uuidgen)
    way_out_lock=/tmp/$(uuidgen)
    exec 4>"$way_in_lock"
    exec 5>"$way_out_lock"
    flock 4
    ( (
    flock -s 5;
    while true; do
        if read -r line ; then
            printf "RUNAWAY_STDOUT: %s\n" "$line" ;
        elif flock -ns 4; then
            flock -u 5;
            break;
        fi;
    done<"$stdout_fifo";
    )&)
    ( (
    flock -s 5;
    while true; do
        if read -r line ; then
            printf "RUNAWAY_STDERR: %s\n" "$line";
        elif flock -ns 4; then
            flock -u 5;
            break;
        fi;
    done<"$stderr_fifo";
    )&)
    eval "$1" 1>"$stdout_fifo" 2>"$stderr_fifo"
    RUNAWAY_ECODE=$?
    flock -u 4
    flock 5
    rm "$way_in_lock"
    rm "$way_out_lock"
    printf 'RUNAWAY_ECODE: %s\n' $RUNAWAY_ECODE
}
