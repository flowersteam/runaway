# Function used to execute a command.
rw_run() {

    # Since stdout and stderr are handled in separate threads, we have to ensure that stdout and
    # stderr were completely read before moving to the next command. We do so by using file locks,
    # which are unix locks using file descriptors.

    # We create the files
    way_in_lock=/tmp/$(uuidgen)
    way_out_lock=/tmp/$(uuidgen)

    # We create the file descriptors
    exec 4>"$way_in_lock"
    exec 5>"$way_out_lock"

    # We lock the way_in_lock in exclusive mode. This will prevent the stdout and stderr handlers to
    # break before the command completed.
    flock 4

    # We start a subcommand that handles the stdout messages on the stdout_fifo.
    # First we lock the way_out_lock in shared mode to prevent the command from returning before all
    # messages were handled.
    ( (
    flock -s 5;
    while true; do
        # We format the line
        if read -r line ; then
            printf "RUNAWAY_STDOUT: %s\n" "$line" ;
        # We try to acquire a shared lock on the way_in_lock. This can only happen when the exclusive
        # lock hold by the command will be released, after the command was executed.
        elif flock -ns 4; then
            # We release our lock on the way_out_lock.
            flock -u 5;
            break;
        fi;
    done<"$stdout_fifo";
    )&)

    # We start the same subcommand for the stderr.
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

    # Now we are ready to evaluate the command. The stdout and stderr are forwarded to the right
    # fifos for further handling under the adequate subprocesses.
    # printf %s\\n "$@"
    # printf %s\\n "$1"

    # We execute the command
    eval "$1" 1>"$stdout_fifo" 2>"$stderr_fifo"

    # We retrieve the exit code
    RUNAWAY_ECODE=$?

    # We release the exclusive lock on the way_in_lock. This has the effect to break the loops in
    # the stdout and stderr handlers.
    flock -u 4

    # We wait to acquire an exclusive lock on the way_out_lock. This can only occur after the two
    # handlers broke and released their shared lock.
    flock 5

    # We remove the locks
    rm "$way_in_lock"
    rm "$way_out_lock"

    # We echo the exit code.
    printf 'RUNAWAY_ECODE: %s\n' $RUNAWAY_ECODE

}
