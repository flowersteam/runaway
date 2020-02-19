rw_init() {
    # We generate two fifo that will be used to format stdout messages and stderr messages
    stdout_fifo="/tmp/$(uuidgen)"
    export stdout_fifo
    stderr_fifo="/tmp/$(uuidgen)"
    export stderr_fifo
    mkfifo "$stdout_fifo"
    mkfifo "$stderr_fifo"
}
