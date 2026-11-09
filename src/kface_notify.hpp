// Tell the user's kface-overlay (if running) what a scan is doing. Best effort, never blocks.
#pragma once

#include <fcntl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

#include <cstdio>
#include <cstring>
#include <string>

namespace kface {

inline std::string overlay_socket(uid_t uid) { return "/run/user/" + std::to_string(uid) + "/kface-overlay.sock"; }

// event: "scan", "ok" or "fail"
inline void notify_overlay(uid_t uid, const char *event)
{
    std::string path = overlay_socket(uid);
    struct stat st;
    // only talk to a real socket owned by the user being authenticated
    if (lstat(path.c_str(), &st) != 0 || !S_ISSOCK(st.st_mode) || st.st_uid != uid) return;
    if (path.size() >= sizeof(sockaddr_un::sun_path)) return;

    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
    if (fd < 0) return;
    sockaddr_un addr{};
    addr.sun_family = AF_UNIX;
    memcpy(addr.sun_path, path.c_str(), path.size());
    if (connect(fd, (sockaddr *)&addr, sizeof addr) == 0) {
        std::string msg = std::string(event) + "\n";
        (void)!send(fd, msg.data(), msg.size(), MSG_NOSIGNAL | MSG_DONTWAIT);
    }
    close(fd);
}

}  // namespace kface
