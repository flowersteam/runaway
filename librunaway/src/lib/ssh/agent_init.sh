rw_init() {
    stdout_fifo="/tmp/$(uuidgen)"
    export stdout_fifo
    stderr_fifo="/tmp/$(uuidgen)"
    export stderr_fifo
    mkfifo "$stdout_fifo"
    mkfifo "$stderr_fifo"
}
