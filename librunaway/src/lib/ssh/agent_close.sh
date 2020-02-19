# Function to teardown the agent.
rw_close(){

    # We output the current working directory
    printf 'RUNAWAY_CWD: %s\n' "$(pwd)"

    # We echo the environment variables
    env | sed 's/^/RUNAWAY_ENV: /g'

    # We cleanup
    rm "$stdout_fifo"
    rm "$stderr_fifo"

    # We leave
    echo 'RUNAWAY_EOF:'
}
