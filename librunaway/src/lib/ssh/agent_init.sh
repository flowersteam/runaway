rw_init() {
    # We generate two fifo that will be used to format stdout messages and stderr messages
    export stdout_fifo=/tmp/$(uuidgen)
    export stderr_fifo=/tmp/$(uuidgen)
    mkfifo $stdout_fifo
    mkfifo $stderr_fifo
}
