/*
 * Moves close() of internet sockets off the caller's thread.
 *
 * On Macs with a network content filter (corporate endpoint security), the
 * kernel can hold close() of a TCP socket for up to a second while the filter
 * finishes with the flow. libslirp closes sockets on its single network
 * thread, so every connection teardown froze all guest networking for ~1 s:
 * new connections waited for their SYN-ACK (the guest saw 1/2/3 s connects)
 * and DNS answers queued up behind it.
 *
 * dyld interposing (the __DATA,__interpose section of the main executable)
 * rebinds close() in every other image, libslirp included. AF_INET/AF_INET6
 * sockets are handed to a background thread through a pipe and closed there;
 * everything else is closed right away. The fd number stays allocated until
 * the real close, so it can't be reused under the caller.
 */
#include <pthread.h>
#include <sys/socket.h>
#include <unistd.h>

static int queue[2] = {-1, -1};
static pthread_once_t once = PTHREAD_ONCE_INIT;

static void *closer(void *arg) {
    (void)arg;
    int fd;
    while (read(queue[0], &fd, sizeof fd) == sizeof fd) {
        close(fd); /* this image isn't interposed: the real close */
    }
    return NULL;
}

static void start(void) {
    pthread_t t;
    if (pipe(queue) != 0) {
        queue[0] = queue[1] = -1;
        return;
    }
    if (pthread_create(&t, NULL, closer, NULL) != 0) {
        close(queue[0]);
        close(queue[1]);
        queue[0] = queue[1] = -1;
        return;
    }
    pthread_detach(t);
}

static int is_inet_socket(int fd) {
    struct sockaddr_storage addr;
    socklen_t len = sizeof addr;
    if (getsockname(fd, (struct sockaddr *)&addr, &len) != 0)
        return 0; /* not a socket, or not open */
    return addr.ss_family == AF_INET || addr.ss_family == AF_INET6;
}

static int orbit_close(int fd) {
    if (fd >= 0 && is_inet_socket(fd)) {
        pthread_once(&once, start);
        if (queue[1] >= 0 && fd != queue[0] && fd != queue[1] &&
            write(queue[1], &fd, sizeof fd) == sizeof fd)
            return 0;
    }
    return close(fd);
}

__attribute__((used)) static struct {
    const void *replacement;
    const void *original;
} interposers[] __attribute__((section("__DATA,__interpose"))) = {
    {(const void *)orbit_close, (const void *)close},
};
