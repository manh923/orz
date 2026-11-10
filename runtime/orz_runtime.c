// orz のランタイム（x86-64 の Linux と Windows）。
//
// C ライブラリは使わない（-ffreestanding でコンパイルする）。OS とのやりとりは「OS」の節にまとめてある。
// Linux ではシステムコール、Windows では kernel32 の関数を使う。
// コンパイラが出したアセンブリから、C と同じ呼び出し方（System V ABI）で呼ばれる。
// Windows でも、-mabi=sysv でコンパイルして同じ呼び出し方にする（kernel32 の関数だけ Windows の呼び出し方）。
//
// 値の持ち方：
//   string  先頭のバイトを指すアドレス。長さはその手前の 8 バイトにあり、最後に 0 が付いている
//   array   先頭の要素を指すアドレス。要素数はその手前の 8 バイトにある
//
// メモリは確保するだけで、プログラムの終わりにまとめて解放する（GC は実装の手順 7 で入れる）。

typedef long long i64;
typedef unsigned long long u64;
typedef u64 usize;

#define PANIC_EXIT_CODE 101

// ---------------- OS ----------------
//
// 失敗したときは負の値（-エラーの番号）を返す。番号は Linux の errno にそろえる。
// fd の 0・1・2 は、標準入力・標準出力・標準エラー出力。

#define ENOENT 2
#define EINTR 4
#define EACCES 13
#define EEXIST 17
#define ENOTDIR 20
#define EISDIR 21
#define EMFILE 24
#define ENOSPC 28
#define EROFS 30
#define ENAMETOOLONG 36
// これより大きい番号は、Windows のエラーの番号に足したもの。
#define WIN_ERROR_BASE 100000

void *memcpy(void *d, const void *s, usize n);
static i64 os_write(i64 fd, const char *p, i64 n);
static i64 os_read(i64 fd, char *p, i64 n);
static i64 os_open_read(const char *path);
static i64 os_open_write(const char *path);
static i64 os_close(i64 fd);
// 0 で埋めたメモリ。失敗なら 0。
static void *os_map(i64 n);
static void os_unmap(void *p, i64 n);
__attribute__((noreturn)) static void os_exit(i64 code);
// fd が画面（端末）か。
static int os_is_terminal(i64 fd);

#ifndef _WIN32

#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_mmap 9
#define SYS_munmap 11
#define SYS_rt_sigaction 13
#define SYS_pipe2 293
#define SYS_fork 57
#define SYS_execve 59
#define SYS_wait4 61
#define SYS_unlink 87
#define SYS_sigaltstack 131
#define SYS_exit_group 231

#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000

static i64 sys3(i64 n, i64 a, i64 b, i64 c) {
    i64 r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static i64 sys4(i64 n, i64 a, i64 b, i64 c, i64 d) {
    i64 r;
    register i64 r10 __asm__("r10") = d;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10) : "rcx", "r11", "memory");
    return r;
}

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    i64 r;
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return r;
}

static i64 os_write(i64 fd, const char *p, i64 n) {
    i64 r;
    do r = sys3(SYS_write, fd, (i64)p, n);
    while (r == -EINTR);
    return r;
}

static i64 os_read(i64 fd, char *p, i64 n) {
    i64 r;
    do r = sys3(SYS_read, fd, (i64)p, n);
    while (r == -EINTR);
    return r;
}

static i64 os_open_read(const char *path) { return sys4(SYS_open, (i64)path, O_RDONLY | O_CLOEXEC, 0, 0); }

static i64 os_open_write(const char *path) {
    return sys4(SYS_open, (i64)path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644, 0);
}

static i64 os_close(i64 fd) { return sys3(SYS_close, fd, 0, 0); }

static void *os_map(i64 n) {
    i64 p = sys6(SYS_mmap, 0, n, 3 /* 読み書き */, 0x22 /* MAP_PRIVATE | MAP_ANONYMOUS */, -1, 0);
    return p < 0 && p > -4096 ? 0 : (void *)p;
}

static void os_unmap(void *p, i64 n) { sys3(SYS_munmap, (i64)p, n, 0); }

__attribute__((noreturn)) static void os_exit(i64 code) {
    for (;;) sys3(SYS_exit_group, code, 0, 0);
}

// 端末の設定を読めれば（ioctl の TCGETS）、端末。
static int os_is_terminal(i64 fd) {
    char settings[64];
    return sys3(16 /* ioctl */, fd, 0x5401, (i64)settings) == 0;
}

#else // _WIN32

#define WINAPI __attribute__((ms_abi))
#define IMPORT __declspec(dllimport)
typedef void *HANDLE;
typedef unsigned int u32;
typedef unsigned short u16;

IMPORT HANDLE WINAPI GetStdHandle(u32 which);
IMPORT int WINAPI GetConsoleMode(HANDLE h, u32 *mode);
IMPORT int WINAPI WriteConsoleW(HANDLE h, const u16 *p, u32 n, u32 *done, void *reserved);
IMPORT int WINAPI ReadConsoleW(HANDLE h, u16 *p, u32 n, u32 *done, void *control);
IMPORT int WINAPI WriteFile(HANDLE h, const void *p, u32 n, u32 *done, void *overlapped);
IMPORT int WINAPI ReadFile(HANDLE h, void *p, u32 n, u32 *done, void *overlapped);
IMPORT HANDLE WINAPI CreateFileW(const u16 *path, u32 access, u32 share, void *security, u32 how, u32 flags,
                                 HANDLE tmpl);
IMPORT u32 WINAPI GetFileAttributesW(const u16 *path);
IMPORT int WINAPI CloseHandle(HANDLE h);
IMPORT void *WINAPI VirtualAlloc(void *p, usize n, u32 type, u32 protect);
IMPORT int WINAPI VirtualFree(void *p, usize n, u32 type);
IMPORT __attribute__((noreturn)) void WINAPI ExitProcess(u32 code);
IMPORT u16 *WINAPI GetCommandLineW(void);
IMPORT int WINAPI WideCharToMultiByte(u32 cp, u32 flags, const u16 *w, int nw, char *s, int ns, const char *def,
                                      int *used);
IMPORT int WINAPI MultiByteToWideChar(u32 cp, u32 flags, const char *s, int ns, u16 *w, int nw);
IMPORT void *WINAPI AddVectoredExceptionHandler(u32 first, i64(WINAPI *handler)(void *info));
IMPORT int WINAPI SetThreadStackGuarantee(u32 *size);
IMPORT u32 WINAPI GetLastError(void);
IMPORT int WINAPI DeleteFileW(const u16 *path);
IMPORT u32 WINAPI GetEnvironmentVariableW(const u16 *name, u16 *buf, u32 size);
IMPORT int WINAPI SetHandleInformation(HANDLE h, u32 mask, u32 flags);
IMPORT int WINAPI CreateProcessW(const u16 *app, u16 *cmdline, void *pattr, void *tattr, int inherit, u32 flags,
                                 void *env, const u16 *dir, void *startup, void *info);
IMPORT u32 WINAPI WaitForSingleObject(HANDLE h, u32 ms);
IMPORT int WINAPI GetExitCodeProcess(HANDLE h, u32 *code);

#define CP_UTF8 65001

// Windows のエラーの番号を、Linux の errno にそろえる。
static i64 win_errno(u32 e) {
    switch (e) {
    case 2:   // ERROR_FILE_NOT_FOUND
    case 3:   // ERROR_PATH_NOT_FOUND
    case 15:  // ERROR_INVALID_DRIVE
    case 53:  // ERROR_BAD_NETPATH
    case 67:  // ERROR_BAD_NET_NAME
    case 123: // ERROR_INVALID_NAME
        return ENOENT;
    case 5:  // ERROR_ACCESS_DENIED
    case 32: // ERROR_SHARING_VIOLATION
    case 33: // ERROR_LOCK_VIOLATION
        return EACCES;
    case 80:  // ERROR_FILE_EXISTS
    case 183: // ERROR_ALREADY_EXISTS
        return EEXIST;
    case 267: // ERROR_DIRECTORY
        return ENOTDIR;
    case 4: // ERROR_TOO_MANY_OPEN_FILES
        return EMFILE;
    case 39:  // ERROR_HANDLE_DISK_FULL
    case 112: // ERROR_DISK_FULL
        return ENOSPC;
    case 19: // ERROR_WRITE_PROTECT
        return EROFS;
    case 111: // ERROR_BUFFER_OVERFLOW
    case 206: // ERROR_FILENAME_EXCED_RANGE
        return ENAMETOOLONG;
    default:
        return WIN_ERROR_BASE + e;
    }
}

static HANDLE os_handle(i64 fd) {
    // STD_INPUT_HANDLE（-10）、STD_OUTPUT_HANDLE（-11）、STD_ERROR_HANDLE（-12）
    if (fd >= 0 && fd <= 2) return GetStdHandle((u32)(-10 - fd));
    return (HANDLE)fd;
}

static int is_console(HANDLE h) {
    u32 mode;
    return GetConsoleMode(h, &mode) != 0;
}

// 画面には、UTF-8 を UTF-16 にして書く（コードページによらず、日本語がそのまま出る）。
// 文字の途中で切れたバイトは、次に書くときまで取っておく。
static char carry[3][4];
static int ncarry[3];
static u16 wbuf[4096];

static int utf8_len(unsigned char c) { return c < 0x80 ? 1 : c >= 0xf0 ? 4 : c >= 0xe0 ? 3 : c >= 0xc0 ? 2 : 1; }

static void console_write(HANDLE h, i64 fd, const char *p, i64 n) {
    char tmp[sizeof wbuf / sizeof wbuf[0]];
    while (n > 0 || ncarry[fd] > 0) {
        i64 k = ncarry[fd];
        memcpy(tmp, carry[fd], k);
        ncarry[fd] = 0;
        i64 take = n < (i64)sizeof tmp - k ? n : (i64)sizeof tmp - k;
        memcpy(tmp + k, p, take);
        p += take;
        n -= take;
        k += take;
        // 最後の文字が途中で切れていれば、取っておく。
        i64 last = k;
        while (last > 0 && last > k - 4 && ((unsigned char)tmp[last - 1] & 0xc0) == 0x80) last--;
        if (last > 0 && last - 1 + utf8_len((unsigned char)tmp[last - 1]) > k) {
            ncarry[fd] = (int)(k - (last - 1));
            memcpy(carry[fd], tmp + last - 1, ncarry[fd]);
            k = last - 1;
            if (n == 0 && k == 0) return;
        }
        int w = MultiByteToWideChar(CP_UTF8, 0, tmp, (int)k, wbuf, sizeof wbuf / sizeof wbuf[0]);
        for (int off = 0; off < w;) {
            u32 done = 0;
            if (!WriteConsoleW(h, wbuf + off, (u32)(w - off), &done, 0) || done == 0) return;
            off += (int)done;
        }
        if (n == 0) return;
    }
}

static i64 os_write(i64 fd, const char *p, i64 n) {
    HANDLE h = os_handle(fd);
    if ((fd == 1 || fd == 2) && is_console(h)) {
        console_write(h, fd, p, n);
        return n;
    }
    u32 done = 0;
    if (!WriteFile(h, p, (u32)(n < 0x40000000 ? n : 0x40000000), &done, 0)) return -win_errno(GetLastError());
    return done;
}

// 画面から読むときは UTF-16 で読んで、UTF-8 にする。行の先頭の Ctrl+Z は入力の終わり。
static i64 console_read(HANDLE h, char *p, i64 n) {
    i64 cap = (i64)(sizeof wbuf / sizeof wbuf[0]);
    u32 max = (u32)(n / 3 < cap ? n / 3 : cap);
    u32 got = 0;
    if (max < 2) max = 2;
    if (!ReadConsoleW(h, wbuf, max - 1, &got, 0)) return -win_errno(GetLastError());
    if (got > 0 && wbuf[got - 1] >= 0xd800 && wbuf[got - 1] < 0xdc00) {
        // サロゲートペアの前半で切れたら、後半も読む。
        u32 more = 0;
        if (ReadConsoleW(h, wbuf + got, 1, &more, 0)) got += more;
    }
    if (got == 0 || wbuf[0] == 0x1a) return 0;
    return WideCharToMultiByte(CP_UTF8, 0, wbuf, (int)got, p, (int)n, 0, 0);
}

static i64 os_read(i64 fd, char *p, i64 n) {
    HANDLE h = os_handle(fd);
    if (fd == 0 && is_console(h)) return console_read(h, p, n);
    u32 done = 0;
    if (!ReadFile(h, p, (u32)(n < 0x40000000 ? n : 0x40000000), &done, 0)) {
        u32 e = GetLastError();
        if (e == 109 /* ERROR_BROKEN_PIPE */ || e == 38 /* ERROR_HANDLE_EOF */) return 0;
        return -win_errno(e);
    }
    return done;
}

// パス（UTF-8 の orz の文字列）を UTF-16 にする。
static u16 wpath[32768];

static int to_wide(const char *path) {
    i64 n = ((const i64 *)path)[-1];
    if (n == 0) {
        wpath[0] = 0;
        return 1;
    }
    if (n > 32767) return 0;
    int w = MultiByteToWideChar(CP_UTF8, 0, path, (int)n, wpath, 32767);
    if (w <= 0) return 0;
    wpath[w] = 0;
    return 1;
}

static i64 open_error(void) {
    u32 e = GetLastError();
    // フォルダを開こうとすると「アクセスが拒否されました」になるので、見分ける。
    if (e == 5) {
        u32 a = GetFileAttributesW(wpath);
        if (a != 0xffffffffu && (a & 0x10 /* FILE_ATTRIBUTE_DIRECTORY */)) return -EISDIR;
    }
    return -win_errno(e);
}

static i64 os_open_read(const char *path) {
    if (!to_wide(path)) return -ENAMETOOLONG;
    // GENERIC_READ、ほかのプログラムも読み書きできるように、OPEN_EXISTING、FILE_ATTRIBUTE_NORMAL
    HANDLE h = CreateFileW(wpath, 0x80000000u, 3, 0, 3, 0x80, 0);
    if (h == (HANDLE)-1) return open_error();
    return (i64)h;
}

static i64 os_open_write(const char *path) {
    if (!to_wide(path)) return -ENAMETOOLONG;
    // GENERIC_WRITE、ほかのプログラムも読めるように、CREATE_ALWAYS、FILE_ATTRIBUTE_NORMAL
    HANDLE h = CreateFileW(wpath, 0x40000000u, 1, 0, 2, 0x80, 0);
    if (h == (HANDLE)-1) return open_error();
    return (i64)h;
}

static i64 os_close(i64 fd) { return CloseHandle((HANDLE)fd) ? 0 : -win_errno(GetLastError()); }

// MEM_COMMIT | MEM_RESERVE、PAGE_READWRITE
static void *os_map(i64 n) { return VirtualAlloc(0, (usize)n, 0x3000, 4); }

// MEM_RELEASE（確保したときの先頭を渡す）
static void os_unmap(void *p, i64 n) {
    (void)n;
    VirtualFree(p, 0, 0x8000);
}

// 終了コードは Linux と同じく 0〜255。
__attribute__((noreturn)) static void os_exit(i64 code) { ExitProcess((u32)(code & 255)); }

static int os_is_terminal(i64 fd) { return is_console(os_handle(fd)); }

// gcc は、4KB より大きいスタックを使う関数の先頭でこれを呼ぶ（スタックを 1 ページずつ触って広げる）。
// コンパイラが出すアセンブリも、大きな関数では同じように呼ぶ。%rax に大きさ。%rsp は変えない。
__asm__(".text\n"
        ".globl ___chkstk_ms\n"
        "___chkstk_ms:\n"
        "\tpushq %rcx\n"
        "\tpushq %rax\n"
        "\tleaq 24(%rsp), %rcx\n"
        "1:\tcmpq $0x1000, %rax\n"
        "\tjb 2f\n"
        "\tsubq $0x1000, %rcx\n"
        "\torq $0, (%rcx)\n"
        "\tsubq $0x1000, %rax\n"
        "\tjmp 1b\n"
        "2:\tsubq %rax, %rcx\n"
        "\torq $0, (%rcx)\n"
        "\tpopq %rax\n"
        "\tpopq %rcx\n"
        "\tret\n");

#endif // _WIN32

// gcc は構造体のコピーなどで、これらを勝手に呼ぶことがある。
void *memcpy(void *d, const void *s, usize n) {
    char *dp = d;
    const char *sp = s;
    while (n--) *dp++ = *sp++;
    return d;
}

void *memset(void *d, int c, usize n) {
    char *dp = d;
    while (n--) *dp++ = (char)c;
    return d;
}

int memcmp(const void *a, const void *b, usize n) {
    const unsigned char *x = a, *y = b;
    for (; n; n--, x++, y++)
        if (*x != *y) return *x - *y;
    return 0;
}

static i64 cstrlen(const char *s) {
    i64 n = 0;
    while (s[n]) n++;
    return n;
}

// ---------------- 長さの付いた文字列 ----------------

#define STATIC_STR(name, lit) \
    static struct { i64 len; char s[sizeof(lit)]; } __attribute__((aligned(8))) name = {sizeof(lit) - 1, lit}

STATIC_STR(str_empty, "");
STATIC_STR(str_true, "true");
STATIC_STR(str_false, "false");

static struct { i64 len; i64 data[1]; } __attribute__((aligned(8))) empty_array = {0, {0}};

static i64 str_len(const char *s) { return ((const i64 *)s)[-1]; }

// ---------------- 出力（バッファ付き） ----------------

static char outbuf[8192];
static i64 outlen;

static void write_all(int fd, const char *p, i64 n) {
    while (n > 0) {
        i64 r = os_write(fd, p, n);
        if (r <= 0) return;
        p += r;
        n -= r;
    }
}

// 書き出す先（ふつうは標準出力。eprint・eput の間だけ標準エラー出力）。
static int out_fd = 1;

void orz_rt_flush(void) {
    write_all(out_fd, outbuf, outlen);
    outlen = 0;
}

static void out(const char *p, i64 n) {
    if (n > (i64)sizeof outbuf - outlen) {
        orz_rt_flush();
        if (n >= (i64)sizeof outbuf) {
            write_all(1, p, n);
            return;
        }
    }
    memcpy(outbuf + outlen, p, n);
    outlen += n;
}

static void outs(const char *s) { out(s, cstrlen(s)); }

static void err(const char *s) { write_all(2, s, cstrlen(s)); }

// 数を 10 進の文字列にする。buf は 21 バイト以上。最小値も正しく扱う。
static char *fmt_long(char *buf, i64 v) {
    char *p = buf + 21;
    *--p = 0;
    u64 u = v < 0 ? -(u64)v : (u64)v;
    do {
        *--p = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    if (v < 0) *--p = '-';
    return p;
}

static char *fmt_hex(char *buf, u64 v) {
    char *p = buf + 21;
    *--p = 0;
    do {
        *--p = "0123456789abcdef"[v & 15];
        v >>= 4;
    } while (v);
    *--p = 'x';
    *--p = '0';
    return p;
}

// ---------------- 終了と実行時エラー ----------------

__attribute__((noreturn)) void orz_rt_exit(i64 code) {
    orz_rt_flush();
    os_exit(code);
}

static void panic_begin(const char *site) {
    orz_rt_flush();
    err("実行時エラー: ");
    if (site) {
        err(site);
        err(": ");
    }
}

__attribute__((noreturn)) static void panic_end(void) {
    err("\n");
    os_exit(PANIC_EXIT_CODE);
}

__attribute__((noreturn)) void orz_rt_panic(const char *site, const char *reason) {
    panic_begin(site);
    err(reason);
    panic_end();
}

__attribute__((noreturn)) void orz_rt_panic_index(const char *site, i64 index, i64 len) {
    char a[24], b[24];
    panic_begin(site);
    err("配列の範囲外です（添字 ");
    err(fmt_long(a, index));
    err("、長さ ");
    err(fmt_long(b, len));
    err("）");
    panic_end();
}

// 変換で範囲外。reason は「8 の範囲（0〜255）に入りません」のような文。
__attribute__((noreturn)) void orz_rt_panic_convert(const char *site, i64 value, const char *reason) {
    char a[24];
    panic_begin(site);
    err("変換する値 ");
    err(fmt_long(a, value));
    err(" が ");
    err(reason);
    panic_end();
}

// ---------------- メモリ ----------------
//
// 保守的なマーク・アンド・スイープのガベージコレクション（GC）。
// スタック、グローバル変数、arena の中身から、ポインタに見える値をたどり、届かないものを解放する。
//
// ヒープは 1MB の塊（chunk）で、塊ごとに 1 つの大きさの区分のものだけを並べる。
// 32KB を超えるものは、1 つずつ mmap する。どの塊のどの場所かは表から引けるので、
// 途中を指すポインタ（bytes(s) や配列の要素のアドレス）からでも、元のものがわかる。
//
// arena の中で確保したものは GC を通らない。arena を抜けるときにまとめて返す。

static void *map(i64 n, const char *site) {
    void *p = os_map(n);
    if (!p) orz_rt_panic(site, "メモリが足りません");
    return p;
}

static void unmap(void *p, i64 n) { os_unmap(p, n); }

#define GC_CHUNK (1LL << 20)
#define GC_HEADER (32LL << 10) // 塊の先頭：3 つのビットマップ（確保済み・印・中を調べない）
#define GC_LARGE (32LL << 10)  // これより大きいものは 1 つずつ mmap する
#define GC_MIN_THRESHOLD (8LL << 20)

static const i64 class_size[] = {
    16,   32,   48,   64,   80,   96,   112,  128,   160,   192,   224,   256,   320,   384,
    448,  512,  640,  768,  896,  1024, 1280, 1536,  1792,  2048,  2560,  3072,  3584,  4096,
    5120, 6144, 7168, 8192, 10240, 12288, 14336, 16384, 20480, 24576, 28672, 32768,
};
#define NCLASS ((int)(sizeof class_size / sizeof class_size[0]))

struct chunk {
    char *start, *end; // 中身の範囲
    i64 objsize;      // 1 つの大きさ（大きいものは全体）
    i64 nobj;
    int cls;           // 大きさの区分（1 つずつ mmap したものは -1）
    unsigned char *alloc, *mark, *noscan;
    unsigned char large_bits[3];
    char *map_start;
    i64 map_len;
};

static struct chunk **chunks; // start の順に並べる
static i64 nchunks, chunks_cap;
static char *heap_lo = (char *)-1, *heap_hi;
static void *freelist[NCLASS];
static i64 gc_since, gc_threshold = GC_MIN_THRESHOLD;
static char *stack_top;

// コンパイラが出す：GC を動かさないか（--no-gc）、グローバル変数の範囲。
extern const i64 orz_gc_disabled;
extern char orz_globals_start[], orz_globals_end[];

static int bit(const unsigned char *b, i64 i) { return (b[i >> 3] >> (i & 7)) & 1; }
static void bit_set(unsigned char *b, i64 i) { b[i >> 3] |= (unsigned char)(1 << (i & 7)); }
static void bit_clear(unsigned char *b, i64 i) { b[i >> 3] &= (unsigned char)~(1 << (i & 7)); }

// 塊の情報を置く場所（GC の対象外）。
static struct chunk *meta_free;
static struct chunk *new_meta(void) {
    if (meta_free) {
        struct chunk *c = meta_free;
        meta_free = *(struct chunk **)c;
        memset(c, 0, sizeof *c);
        return c;
    }
    static char *cur, *end;
    if (end - cur < (i64)sizeof(struct chunk)) {
        cur = map(1LL << 16, 0);
        end = cur + (1LL << 16);
    }
    struct chunk *c = (struct chunk *)cur;
    cur += (sizeof(struct chunk) + 15) & ~15ULL;
    return c;
}

static void add_chunk(struct chunk *c) {
    if (nchunks == chunks_cap) {
        i64 cap = chunks_cap ? chunks_cap * 2 : 256;
        struct chunk **n = map(cap * (i64)sizeof *n, 0);
        memcpy(n, chunks, nchunks * sizeof *n);
        if (chunks) unmap(chunks, chunks_cap * (i64)sizeof *n);
        chunks = n;
        chunks_cap = cap;
    }
    i64 i = nchunks;
    while (i > 0 && chunks[i - 1]->start > c->start) {
        chunks[i] = chunks[i - 1];
        i--;
    }
    chunks[i] = c;
    nchunks++;
    if (c->start < heap_lo) heap_lo = c->start;
    if (c->end > heap_hi) heap_hi = c->end;
}

static struct chunk *find_chunk(const char *p) {
    if (p < heap_lo || p >= heap_hi) return 0;
    i64 lo = 0, hi = nchunks - 1;
    while (lo <= hi) {
        i64 mid = (lo + hi) / 2;
        struct chunk *c = chunks[mid];
        if (p < c->start)
            hi = mid - 1;
        else if (p >= c->end)
            lo = mid + 1;
        else
            return c;
    }
    return 0;
}

static void new_chunk(int cls, const char *site) {
    char *m = map(GC_CHUNK, site);
    struct chunk *c = new_meta();
    c->objsize = class_size[cls];
    c->nobj = (GC_CHUNK - GC_HEADER) / c->objsize;
    c->cls = cls;
    c->alloc = (unsigned char *)m;
    c->mark = c->alloc + (8LL << 10);
    c->noscan = c->mark + (8LL << 10);
    c->start = m + GC_HEADER;
    c->end = c->start + c->nobj * c->objsize;
    c->map_start = m;
    c->map_len = GC_CHUNK;
    add_chunk(c);
    // 空いているものをつなぐ（後ろから入れるので、前から使われる）。
    for (i64 i = c->nobj - 1; i >= 0; i--) {
        void **o = (void **)(c->start + i * c->objsize);
        *o = freelist[cls];
        freelist[cls] = o;
    }
}

static int size_class(i64 n) {
    int lo = 0, hi = NCLASS - 1;
    while (lo < hi) {
        int mid = (lo + hi) / 2;
        if (class_size[mid] >= n)
            hi = mid;
        else
            lo = mid + 1;
    }
    return lo;
}

// ---- 印を付ける ----

static void **mstack;
static i64 msp, mcap;

static void mark_ptr(const char *p) {
    struct chunk *c = find_chunk(p);
    if (!c) return;
    i64 i = (p - c->start) / c->objsize;
    if (i >= c->nobj || !bit(c->alloc, i) || bit(c->mark, i)) return;
    bit_set(c->mark, i);
    if (bit(c->noscan, i)) return;
    if (msp == mcap) {
        i64 cap = mcap ? mcap * 2 : 4096;
        void **n = map(cap * (i64)sizeof *n, 0);
        memcpy(n, mstack, msp * sizeof *n);
        if (mstack) unmap(mstack, mcap * (i64)sizeof *n);
        mstack = n;
        mcap = cap;
    }
    mstack[msp++] = c->start + i * c->objsize;
}

static void scan_range(const char *lo, const char *hi) {
    lo = (const char *)(((u64)lo + 7) & ~7ULL);
    for (const char *p = lo; p + 8 <= hi; p += 8) mark_ptr(*(char *const *)p);
}

static void scan_arenas(void);

void orz_rt_collect(void) {
    if (orz_gc_disabled) return;
    // 呼び出し元がレジスタに持っているかもしれないポインタを、スタックに書き出してから調べる。
    i64 regs[6];
    __asm__ volatile("movq %%rbx, 0(%0)\n\t"
                     "movq %%rbp, 8(%0)\n\t"
                     "movq %%r12, 16(%0)\n\t"
                     "movq %%r13, 24(%0)\n\t"
                     "movq %%r14, 32(%0)\n\t"
                     "movq %%r15, 40(%0)"
                     :
                     : "r"(regs)
                     : "memory");
    char *sp;
    __asm__ volatile("movq %%rsp, %0" : "=r"(sp));
    scan_range(sp, stack_top);
    scan_range(orz_globals_start, orz_globals_end);
    scan_arenas();
    while (msp > 0) {
        char *o = mstack[--msp];
        struct chunk *c = find_chunk(o);
        scan_range(o, o + c->objsize);
    }
    __asm__ volatile("" : : "r"(regs) : "memory");

    // 掃除：印のないものを解放し、空いているものをつなぎ直す。
    for (int k = 0; k < NCLASS; k++) freelist[k] = 0;
    i64 live = 0, kept = 0;
    for (i64 j = 0; j < nchunks; j++) {
        struct chunk *c = chunks[j];
        i64 used = 0;
        for (i64 i = 0; i < c->nobj; i++) {
            if (bit(c->alloc, i) && !bit(c->mark, i)) bit_clear(c->alloc, i);
            if (bit(c->alloc, i)) used++;
        }
        if (used == 0) {
            // 何も残っていない塊は、OS に返す。
            unmap(c->map_start, c->map_len);
            *(struct chunk **)c = meta_free;
            meta_free = c;
            continue;
        }
        memset(c->mark, 0, (c->nobj + 7) / 8);
        live += used * c->objsize;
        if (c->cls >= 0) {
            for (i64 i = c->nobj - 1; i >= 0; i--) {
                if (!bit(c->alloc, i)) {
                    void **o = (void **)(c->start + i * c->objsize);
                    *o = freelist[c->cls];
                    freelist[c->cls] = o;
                }
            }
        }
        chunks[kept++] = c;
    }
    nchunks = kept;
    heap_lo = (char *)-1;
    heap_hi = 0;
    for (i64 j = 0; j < nchunks; j++) {
        if (chunks[j]->start < heap_lo) heap_lo = chunks[j]->start;
        if (chunks[j]->end > heap_hi) heap_hi = chunks[j]->end;
    }
    gc_since = 0;
    gc_threshold = live * 2 > GC_MIN_THRESHOLD ? live * 2 : GC_MIN_THRESHOLD;
}

// ---- 確保 ----

// GC のヒープから確保する。中身は 0。noscan なら、中にポインタがない（GC が中を調べない）。
static void *gc_alloc(i64 n, int noscan, const char *site) {
    if (n > (1LL << 46)) orz_rt_panic(site, "メモリが足りません");
    n += 1; // ちょうど終わりを指すポインタも、このものを指しているとみなせるように
    if (gc_since >= gc_threshold) orz_rt_collect();
    if (n > GC_LARGE) {
        i64 len = (n + 4095) & ~4095LL;
        if (gc_since + len >= gc_threshold) orz_rt_collect();
        char *m = map(len, site);
        struct chunk *c = new_meta();
        c->objsize = len;
        c->nobj = 1;
        c->cls = -1;
        c->alloc = &c->large_bits[0];
        c->mark = &c->large_bits[1];
        c->noscan = &c->large_bits[2];
        c->start = m;
        c->end = m + len;
        c->map_start = m;
        c->map_len = len;
        bit_set(c->alloc, 0);
        if (noscan) bit_set(c->noscan, 0);
        add_chunk(c);
        gc_since += len;
        return m;
    }
    int cls = size_class(n);
    if (!freelist[cls]) new_chunk(cls, site);
    void **o = freelist[cls];
    freelist[cls] = *o;
    struct chunk *c = find_chunk((char *)o);
    i64 i = ((char *)o - c->start) / c->objsize;
    bit_set(c->alloc, i);
    if (noscan)
        bit_set(c->noscan, i);
    else
        bit_clear(c->noscan, i);
    memset(o, 0, c->objsize);
    gc_since += c->objsize;
    return o;
}

// ---- arena ----

struct arena_block {
    struct arena_block *next;
    char *cur, *end;
    i64 len;
};

static struct arena_block **arenas; // 開いている arena ごとの、ブロックのつながり
static i64 narenas, arenas_cap;

void orz_rt_arena_push(void) {
    if (narenas == arenas_cap) {
        i64 cap = arenas_cap ? arenas_cap * 2 : 64;
        struct arena_block **n = map(cap * (i64)sizeof *n, 0);
        memcpy(n, arenas, narenas * sizeof *n);
        if (arenas) unmap(arenas, arenas_cap * (i64)sizeof *n);
        arenas = n;
        arenas_cap = cap;
    }
    arenas[narenas++] = 0;
}

void orz_rt_arena_pop(void) {
    struct arena_block *b = arenas[--narenas];
    while (b) {
        struct arena_block *next = b->next;
        unmap(b, b->len);
        b = next;
    }
}

static void *arena_alloc(i64 n, const char *site) {
    if (n > (1LL << 46)) orz_rt_panic(site, "メモリが足りません");
    n = (n + 1 + 15) & ~15LL;
    struct arena_block *b = arenas[narenas - 1];
    if (!b || b->end - b->cur < n) {
        i64 len = n + (i64)sizeof *b > (1LL << 16) ? (n + (i64)sizeof *b + 4095) & ~4095LL : (1LL << 16);
        struct arena_block *nb = map(len, site);
        nb->len = len;
        nb->cur = (char *)nb + ((sizeof *nb + 15) & ~15ULL);
        nb->end = (char *)nb + len;
        nb->next = b;
        arenas[narenas - 1] = nb;
        b = nb;
    }
    void *r = b->cur;
    b->cur += n;
    return r;
}

// arena の中身も GC の根にする（arena のものが GC のものを指していることがある）。
static void scan_arenas(void) {
    for (i64 k = 0; k < narenas; k++)
        for (struct arena_block *b = arenas[k]; b; b = b->next)
            scan_range((char *)b + ((sizeof *b + 15) & ~15ULL), b->cur);
}

// 確保する。arena の中なら arena から、そうでなければ GC のヒープから。中身は 0。
static void *alloc_obj(i64 n, int noscan, const char *site) {
    if (narenas > 0) return arena_alloc(n, site);
    return gc_alloc(n, noscan, site);
}

// allocate と構造体。中にポインタがあるかもしれないので、GC は中を調べる。
void *orz_rt_alloc(i64 n, const char *site) {
    if (n < 0) {
        char a[24];
        panic_begin(site);
        err("allocate に負の大きさ ");
        err(fmt_long(a, n));
        err(" が渡されました");
        panic_end();
    }
    return alloc_obj(n, 0, site);
}

static char *new_str(i64 len) {
    i64 *h = alloc_obj(8 + len + 1, 1, 0);
    h[0] = len;
    return (char *)(h + 1);
}

static char *str_from(const char *p, i64 len) {
    char *s = new_str(len);
    memcpy(s, p, len);
    return s;
}

// 配列を作る。kind：0 = 中身は 0、1 = 空の文字列、2 = 空の配列。scan：要素にポインタがあるか。
void *orz_rt_array_new(i64 count, i64 elem_size, i64 kind, i64 scan, const char *site) {
    if (count < 0) {
        char a[24];
        panic_begin(site);
        err("配列の長さ ");
        err(fmt_long(a, count));
        err(" が負です");
        panic_end();
    }
    if (count > (1LL << 40) / elem_size) orz_rt_panic(site, "メモリが足りません");
    i64 *h = alloc_obj(8 + count * elem_size, !scan, site);
    h[0] = count;
    void **data = (void **)(h + 1);
    if (kind == 1)
        for (i64 i = 0; i < count; i++) data[i] = str_empty.s;
    else if (kind == 2)
        for (i64 i = 0; i < count; i++) data[i] = empty_array.data;
    return data;
}

// array(n, 初期値)：すべての要素に v を入れる。
void orz_rt_array_fill(char *a, i64 v, i64 elem_size) {
    i64 n = ((const i64 *)a)[-1];
    for (i64 i = 0; i < n; i++) {
        if (elem_size == 8)
            ((i64 *)a)[i] = v;
        else if (elem_size == 4)
            ((int *)a)[i] = (int)v;
        else
            a[i] = (char)v;
    }
}

// ---------------- 文字列 ----------------

char *orz_rt_str_concat(const char *a, const char *b) {
    i64 la = str_len(a), lb = str_len(b);
    char *s = new_str(la + lb);
    memcpy(s, a, la);
    memcpy(s + la, b, lb);
    return s;
}

// 中身が同じか。どちらかが null（0）のときは、両方 null なら同じ。
i64 orz_rt_str_eq(const char *a, const char *b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    i64 la = str_len(a);
    return la == str_len(b) && memcmp(a, b, la) == 0;
}

char *orz_rt_str_from_int(i64 v) {
    char buf[24];
    char *p = fmt_long(buf, v);
    return str_from(p, cstrlen(p));
}

char *orz_rt_str_from_bool(i64 v) { return v ? str_true.s : str_false.s; }

// ---------------- 大きな整数（decnum の表示と読み取りに使う） ----------------

#define BIG_WORDS 160

typedef struct {
    int n; // 使っている語の数（上の語は 0 でない）
    unsigned d[BIG_WORDS];
} big;

static void big_set(big *a, u64 v) {
    a->n = 0;
    while (v) {
        a->d[a->n++] = (unsigned)v;
        v >>= 32;
    }
}

static void big_norm(big *a) {
    while (a->n > 0 && a->d[a->n - 1] == 0) a->n--;
}

static int big_cmp(const big *a, const big *b) {
    if (a->n != b->n) return a->n < b->n ? -1 : 1;
    for (int i = a->n - 1; i >= 0; i--)
        if (a->d[i] != b->d[i]) return a->d[i] < b->d[i] ? -1 : 1;
    return 0;
}

static void big_mul_small(big *a, unsigned m) {
    u64 carry = 0;
    for (int i = 0; i < a->n; i++) {
        u64 t = (u64)a->d[i] * m + carry;
        a->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    if (carry) a->d[a->n++] = (unsigned)carry;
}

static void big_add_small(big *a, unsigned v) {
    u64 carry = v;
    for (int i = 0; i < a->n && carry; i++) {
        u64 t = (u64)a->d[i] + carry;
        a->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    if (carry) a->d[a->n++] = (unsigned)carry;
}

// r = a + b（r は a と同じでもよい）
static void big_add(big *r, const big *a, const big *b) {
    int n = a->n > b->n ? a->n : b->n;
    u64 carry = 0;
    for (int i = 0; i < n; i++) {
        u64 t = carry + (i < a->n ? a->d[i] : 0) + (i < b->n ? b->d[i] : 0);
        r->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    r->n = n;
    if (carry) r->d[r->n++] = (unsigned)carry;
}

// a -= b（a >= b）
static void big_sub(big *a, const big *b) {
    i64 borrow = 0;
    for (int i = 0; i < a->n; i++) {
        i64 t = (i64)a->d[i] - (i < b->n ? (i64)b->d[i] : 0) - borrow;
        borrow = t < 0;
        if (t < 0) t += 1LL << 32;
        a->d[i] = (unsigned)t;
    }
    big_norm(a);
}

static void big_shl(big *a, int bits) {
    if (a->n == 0) return;
    int words = bits / 32, s = bits % 32;
    if (s) {
        unsigned carry = 0;
        for (int i = 0; i < a->n; i++) {
            unsigned w = a->d[i];
            a->d[i] = (w << s) | carry;
            carry = w >> (32 - s);
        }
        if (carry) a->d[a->n++] = carry;
    }
    if (words) {
        for (int i = a->n - 1; i >= 0; i--) a->d[i + words] = a->d[i];
        for (int i = 0; i < words; i++) a->d[i] = 0;
        a->n += words;
    }
}

static int big_bitlen(const big *a) {
    if (!a->n) return 0;
    unsigned top = a->d[a->n - 1];
    int b = 0;
    while (top) {
        b++;
        top >>= 1;
    }
    return (a->n - 1) * 32 + b;
}

// n を d で割った商（2^60 未満であること）。n は余りになる。
static u64 big_div(big *n, const big *d, int *rem_nonzero) {
    u64 q = 0;
    int shift = big_bitlen(n) - big_bitlen(d);
    big t;
    for (int i = shift; i >= 0; i--) {
        t = *d;
        big_shl(&t, i);
        if (big_cmp(n, &t) >= 0) {
            big_sub(n, &t);
            q |= 1ULL << i;
        }
    }
    *rem_nonzero = n->n != 0;
    return q;
}

// ---------------- decnum ----------------

static double from_bits(i64 v) {
    union {
        i64 l;
        double d;
    } u = {v};
    return u.d;
}

static i64 to_bits(double d) {
    union {
        double d;
        i64 l;
    } u = {d};
    return u.l;
}

// 元の値に戻せるいちばん短い 10 進の桁を作る（Burger & Dybvig の方法）。
// 値 = f × 2^e（f > 0）。値 = 0.d1d2… × 10^k となる k を返し、桁は buf に入る。
static int dec_digits(u64 f, int e, char *buf, int *k_out) {
    big r, s, mp, mm, t;
    int even = (f % 2 == 0); // 偶数なら、ちょうど境目も元の値に戻る
    if (e >= 0) {
        if (f != (1ULL << 52)) {
            big_set(&r, f);
            big_shl(&r, e + 1);
            big_set(&s, 2);
            big_set(&mp, 1);
            big_shl(&mp, e);
            big_set(&mm, 1);
            big_shl(&mm, e);
        } else {
            big_set(&r, f);
            big_shl(&r, e + 2);
            big_set(&s, 4);
            big_set(&mp, 1);
            big_shl(&mp, e + 1);
            big_set(&mm, 1);
            big_shl(&mm, e);
        }
    } else {
        if (e == -1074 || f != (1ULL << 52)) {
            big_set(&r, f);
            big_shl(&r, 1);
            big_set(&s, 1);
            big_shl(&s, -e + 1);
            big_set(&mp, 1);
            big_set(&mm, 1);
        } else {
            big_set(&r, f);
            big_shl(&r, 2);
            big_set(&s, 1);
            big_shl(&s, -e + 2);
            big_set(&mp, 2);
            big_set(&mm, 1);
        }
    }
    int k = 0;
    for (;;) {
        big_add(&t, &r, &mp);
        int c = big_cmp(&t, &s);
        if (even ? c >= 0 : c > 0) {
            big_mul_small(&s, 10);
            k++;
            continue;
        }
        big_mul_small(&t, 10);
        c = big_cmp(&t, &s);
        if (even ? c < 0 : c <= 0) {
            big_mul_small(&r, 10);
            big_mul_small(&mp, 10);
            big_mul_small(&mm, 10);
            k--;
            continue;
        }
        break;
    }
    int n = 0;
    for (;;) {
        big_mul_small(&r, 10);
        big_mul_small(&mp, 10);
        big_mul_small(&mm, 10);
        int d = 0;
        while (big_cmp(&r, &s) >= 0) {
            big_sub(&r, &s);
            d++;
        }
        int c1 = big_cmp(&r, &mm);
        int low = even ? c1 <= 0 : c1 < 0;
        big_add(&t, &r, &mp);
        int c2 = big_cmp(&t, &s);
        int high = even ? c2 >= 0 : c2 > 0;
        if (!low && !high) {
            buf[n++] = (char)('0' + d);
            continue;
        }
        if (low && high) {
            // どちらの桁でも元に戻るなら、近い方。ちょうど真ん中なら偶数の方（Python や Rust と同じ）。
            big_add(&t, &r, &r);
            int c = big_cmp(&t, &s);
            if (c > 0 || (c == 0 && (d & 1))) d++;
        } else if (high) {
            d++;
        }
        buf[n++] = (char)('0' + d);
        break;
    }
    *k_out = k;
    return n;
}

// decnum を文字列にする。out は 48 バイト以上。
// 1e-4 以上 1e16 未満は 123.45 の形、それ以外は 1.5e20 の形。整数でも .0 を付ける。
static void fmt_dec(char *out, double x) {
    u64 bits = (u64)to_bits(x);
    char *p = out;
    if (bits >> 63) *p++ = '-';
    bits &= ~(1ULL << 63);
    if (bits == 0) {
        memcpy(p, "0.0", 4);
        return;
    }
    int be = (int)(bits >> 52);
    u64 f = bits & ((1ULL << 52) - 1);
    int e;
    if (be == 0) {
        e = -1074;
    } else {
        f |= 1ULL << 52;
        e = be - 1075;
    }
    char dg[24];
    int k;
    int n = dec_digits(f, e, dg, &k);
    int ex = k - 1; // 1.23e(ex) の形の指数
    if (ex >= -4 && ex < 16) {
        if (k <= 0) {
            *p++ = '0';
            *p++ = '.';
            for (int i = 0; i < -k; i++) *p++ = '0';
            for (int i = 0; i < n; i++) *p++ = dg[i];
        } else if (k >= n) {
            for (int i = 0; i < n; i++) *p++ = dg[i];
            for (int i = n; i < k; i++) *p++ = '0';
            *p++ = '.';
            *p++ = '0';
        } else {
            for (int i = 0; i < k; i++) *p++ = dg[i];
            *p++ = '.';
            for (int i = k; i < n; i++) *p++ = dg[i];
        }
    } else {
        *p++ = dg[0];
        if (n > 1) {
            *p++ = '.';
            for (int i = 1; i < n; i++) *p++ = dg[i];
        }
        *p++ = 'e';
        char a[24];
        char *num = fmt_long(a, ex);
        while (*num) *p++ = *num++;
    }
    *p = 0;
}

// 10 進の文字列を decnum にする（正しく丸める）。形が違うか、大きすぎるなら 0 を返す。
static int parse_dec(const char *s, i64 len, double *out) {
    i64 i = 0;
    int neg = 0;
    if (i < len && (s[i] == '+' || s[i] == '-')) neg = s[i++] == '-';
    big num, den;
    big_set(&num, 0);
    int ndig = 0, any = 0, sticky = 0;
    i64 exp10 = 0; // 値 = num × 10^exp10
    while (i < len && s[i] >= '0' && s[i] <= '9') {
        any = 1;
        if (ndig < 800) {
            if (ndig > 0 || s[i] != '0') {
                big_mul_small(&num, 10);
                big_add_small(&num, (unsigned)(s[i] - '0'));
                ndig++;
            }
        } else {
            exp10++;
            if (s[i] != '0') sticky = 1;
        }
        i++;
    }
    if (i < len && s[i] == '.') {
        i++;
        while (i < len && s[i] >= '0' && s[i] <= '9') {
            any = 1;
            if (ndig < 800) {
                if (ndig > 0 || s[i] != '0') {
                    big_mul_small(&num, 10);
                    big_add_small(&num, (unsigned)(s[i] - '0'));
                    ndig++;
                }
                exp10--;
            } else if (s[i] != '0') {
                sticky = 1;
            }
            i++;
        }
    }
    if (!any) return 0;
    if (i < len && (s[i] == 'e' || s[i] == 'E')) {
        i++;
        int eneg = 0;
        if (i < len && (s[i] == '+' || s[i] == '-')) eneg = s[i++] == '-';
        if (i >= len || s[i] < '0' || s[i] > '9') return 0;
        i64 ev = 0;
        while (i < len && s[i] >= '0' && s[i] <= '9') {
            if (ev < 100000) ev = ev * 10 + (s[i] - '0');
            i++;
        }
        exp10 += eneg ? -ev : ev;
    }
    if (i != len) return 0;
    if (num.n == 0) {
        *out = neg ? -0.0 : 0.0;
        return 1;
    }
    if (sticky) {
        // 捨てた桁に 0 でないものがあった印（丸める向きを正しくするため）。
        big_mul_small(&num, 10);
        big_add_small(&num, 1);
        exp10--;
        ndig++;
    }
    i64 mag = ndig + exp10; // 値 < 10^mag
    if (mag > 310) return 0;
    if (mag < -330) {
        *out = neg ? -0.0 : 0.0;
        return 1;
    }
    big_set(&den, 1);
    for (i64 k = 0; k < exp10; k++) big_mul_small(&num, 10);
    for (i64 k = 0; k < -exp10; k++) big_mul_small(&den, 10);
    // q = floor(num × 2^sh / den) が 2^53 以上 2^54 未満になる sh を探す。
    int sh = 54 - (big_bitlen(&num) - big_bitlen(&den));
    u64 q;
    int rem;
    for (;;) {
        big n = num, d = den;
        if (sh >= 0)
            big_shl(&n, sh);
        else
            big_shl(&d, -sh);
        q = big_div(&n, &d, &rem);
        if (q >= (1ULL << 54)) {
            sh--;
            continue;
        }
        if (q < (1ULL << 53)) {
            sh++;
            continue;
        }
        break;
    }
    i64 e2 = 1 - sh; // 値 ≒ (q / 2) × 2^e2
    u64 bits;
    if (e2 < -1074) {
        // 非正規化数：2^-1074 の単位で丸め直す。
        big n = num, d = den;
        big_shl(&n, 1075);
        q = big_div(&n, &d, &rem);
        u64 m = q >> 1;
        if ((q & 1) && (rem || (m & 1))) m++;
        bits = m;
    } else {
        u64 m = q >> 1;
        if ((q & 1) && (rem || (m & 1))) m++;
        if (m == (1ULL << 53)) {
            m >>= 1;
            e2++;
        }
        if (e2 > 971) return 0;
        bits = ((u64)(e2 + 1075) << 52) | (m - (1ULL << 52));
    }
    if (neg) bits |= 1ULL << 63;
    *out = from_bits((i64)bits);
    return 1;
}

char *orz_rt_str_from_dec(i64 v) {
    char buf[48];
    fmt_dec(buf, from_bits(v));
    return str_from(buf, cstrlen(buf));
}

// maybe:32 などの箱。中身は変わらないので、arena の中でも GC のヒープに置く（arena を抜けても消えない）。
void *orz_rt_box(i64 v) {
    i64 *p = gc_alloc(8, 0, 0);
    *p = v;
    return p;
}

// 文字列 → decnum。読めなければ null。
void *orz_rt_str_to_dec(const char *s) {
    double d;
    if (!parse_dec(s, str_len(s), &d)) return 0;
    return orz_rt_box(to_bits(d));
}

// 文字列 → 整数（bits は 8・32・64）。読めないか範囲外なら null。
void *orz_rt_str_to_int(const char *s, i64 bits) {
    i64 len = str_len(s), i = 0;
    int neg = 0;
    if (i < len && (s[i] == '+' || s[i] == '-')) neg = s[i++] == '-';
    if (i >= len) return 0;
    u64 v = 0, limit = neg ? (1ULL << 63) : (1ULL << 63) - 1;
    for (; i < len; i++) {
        if (s[i] < '0' || s[i] > '9') return 0;
        unsigned d = (unsigned)(s[i] - '0');
        if (v > (limit - d) / 10) return 0;
        v = v * 10 + d;
    }
    i64 x = neg ? (i64)(0 - v) : (i64)v;
    if (bits == 8 && (x < 0 || x > 255)) return 0;
    if (bits == 32 && (x < -2147483647LL - 1 || x > 2147483647LL)) return 0;
    return orz_rt_box(x);
}

// x87 の fprem で、誤差なしの余りを出す（C の fmod と同じ。符号は割られる数と同じ）。
i64 orz_rt_fmod(i64 a, i64 b) {
    long double r;
    __asm__("1: fprem\n\t"
            "fnstsw %%ax\n\t"
            "testw $0x400, %%ax\n\t"
            "jnz 1b"
            : "=t"(r)
            : "0"((long double)from_bits(a)), "u"((long double)from_bits(b))
            : "ax", "cc");
    return to_bits((double)r);
}

static long double x87_log2(long double x) {
    long double r;
    __asm__("fyl2x" : "=t"(r) : "0"(x), "u"(1.0L) : "st(1)");
    return r;
}

static long double x87_exp2(long double z) {
    long double ip, t, r;
    __asm__("frndint" : "=t"(ip) : "0"(z));
    __asm__("f2xm1" : "=t"(t) : "0"(z - ip));
    t += 1.0L;
    __asm__("fscale" : "=t"(r) : "0"(t), "u"(ip));
    return r;
}

static int is_integer(double y) {
    double a = y < 0 ? -y : y;
    return a >= 4503599627370496.0 || (double)(i64)y == y;
}

static int is_odd_integer(double y) {
    double a = y < 0 ? -y : y;
    return a < 9007199254740992.0 && (double)(i64)y == y && ((i64)y & 1);
}

static const char dec_overflow[] = "計算の結果が decnum の範囲を超えました";

// decnum の累乗。
i64 orz_rt_fpow(i64 a, i64 b, const char *site) {
    double x = from_bits(a), y = from_bits(b), r;
    if (y == 0) {
        r = 1.0;
    } else if (x == 0) {
        if (y < 0) orz_rt_panic(site, "0 を負の数で累乗しました");
        r = is_odd_integer(y) ? x : 0.0;
    } else if (y == 0.5 && x > 0) {
        // 平方根は、CPU の命令で正確に（正しく丸めて）求める。
        __asm__("sqrtsd %1, %0" : "=x"(r) : "x"(x));
    } else {
        int neg = 0;
        long double ax = x;
        if (x < 0) {
            if (!is_integer(y)) orz_rt_panic(site, "負の数を、整数でない数で累乗しました");
            neg = is_odd_integer(y);
            ax = -ax;
        }
        long double res;
        double ay = y < 0 ? -y : y;
        if (is_integer(y) && ay <= 1024) {
            // 小さな整数の指数は、かけ算を繰り返す（80bit で計算するので誤差が小さい）。
            u64 n = (u64)ay;
            long double base = ax, acc = 1.0L;
            while (n) {
                if (n & 1) acc *= base;
                n >>= 1;
                if (n) base *= base;
            }
            res = y < 0 ? 1.0L / acc : acc;
        } else {
            res = x87_exp2((long double)y * x87_log2(ax));
        }
        r = (double)(neg ? -res : res);
    }
    u64 bits = (u64)to_bits(r);
    if (((bits >> 52) & 0x7ff) == 0x7ff) orz_rt_panic(site, dec_overflow);
    return to_bits(r);
}

__attribute__((noreturn)) void orz_rt_panic_convert_dec(const char *site, i64 value, const char *reason) {
    char a[48];
    fmt_dec(a, from_bits(value));
    panic_begin(site);
    err("変換する値 ");
    err(a);
    err(" が ");
    err(reason);
    panic_end();
}

// ---------------- 表示 ----------------

static void put_int(i64 v) {
    char buf[24];
    outs(fmt_long(buf, v));
}

static void put_addr(i64 v) {
    char buf[24];
    outs(fmt_hex(buf, (u64)v));
}

static void put_str(const char *s) {
    if (!s)
        outs("null");
    else
        out(s, str_len(s));
}

// 標準出力が画面なら、1 行ごとに書き出す（待っている間も、表示が見えるように）。
// ファイルやパイプなら、まとめて書き出す（速い）。
static int out_is_terminal = -1;

void orz_rt_newline(void) {
    out("\n", 1);
    if (out_fd == 1) {
        if (out_is_terminal < 0) out_is_terminal = os_is_terminal(1);
        if (out_is_terminal) orz_rt_flush();
    }
}

// 構造体を表示するための説明。コンパイラが出す。
extern const char *const orz_struct_descs[];

static i64 elem_size(const char *desc) {
    switch (desc[0]) {
    case 'b':
    case 'B':
        return 1;
    case 'i':
        return 4;
    default:
        return 8;
    }
}

static int put_depth;

// 型の説明 desc に従って、p にある値を 1 つ表示する。
//   b = 8、i = 32、l = 64、B = bool、d = decnum、s = string、p = pointer・address、n = null、
//   m = maybe（アドレスの型）、M = maybe（箱に入れた値）、[ = array、S番号; = 構造体
static void put_value(const char *desc, const void *p, int quote) {
    switch (desc[0]) {
    case 'b':
        put_int(*(const unsigned char *)p);
        break;
    case 'B':
        outs(*(const unsigned char *)p ? "true" : "false");
        break;
    case 'i':
        put_int(*(const int *)p);
        break;
    case 'l':
        put_int(*(const i64 *)p);
        break;
    case 'd': {
        char buf[48];
        fmt_dec(buf, from_bits(*(const i64 *)p));
        outs(buf);
        break;
    }
    case 'p':
        put_addr(*(const i64 *)p);
        break;
    case 'n':
        outs("null");
        break;
    case 's': {
        const char *s = *(const char *const *)p;
        if (quote && s) out("\"", 1);
        put_str(s);
        if (quote && s) out("\"", 1);
        break;
    }
    case 'm':
        if (*(const void *const *)p == 0)
            outs("null");
        else
            put_value(desc + 1, p, quote);
        break;
    case 'M': {
        const void *cell = *(const void *const *)p;
        if (!cell)
            outs("null");
        else
            put_value(desc + 1, cell, quote);
        break;
    }
    case '[': {
        const char *a = *(const char *const *)p;
        if (!a) {
            outs("null");
            break;
        }
        if (put_depth > 20) {
            outs("[…]");
            break;
        }
        put_depth++;
        i64 n = ((const i64 *)a)[-1], size = elem_size(desc + 1);
        out("[", 1);
        for (i64 i = 0; i < n; i++) {
            if (i) out(", ", 2);
            put_value(desc + 1, a + i * size, 1);
        }
        out("]", 1);
        put_depth--;
        break;
    }
    case 'S': {
        const char *obj = *(const char *const *)p;
        i64 index = 0;
        for (const char *q = desc + 1; *q != ';'; q++) index = index * 10 + (*q - '0');
        const char *sd = orz_struct_descs[index];
        outs(sd); // 構造体の名前
        if (put_depth > 20) {
            outs("(…)");
            break;
        }
        put_depth++;
        out("(", 1);
        const char *f = sd + cstrlen(sd) + 1;
        int first = 1;
        while (*f) {
            const char *fname = f;
            const char *info = fname + cstrlen(fname) + 1; // 「位置:型の説明」
            i64 offset = 0;
            const char *q = info;
            while (*q != ':') offset = offset * 10 + (*q++ - '0');
            if (!first) out(", ", 2);
            first = 0;
            outs(fname);
            out(": ", 2);
            put_value(q + 1, obj + offset, 1);
            f = info + cstrlen(info) + 1;
        }
        out(")", 1);
        put_depth--;
        break;
    }
    }
}

// v を、型の説明 desc に従って表示する。
void orz_rt_put(i64 v, const char *desc) { put_value(desc, &v, 0); }

// stdio.eprint・eput：標準エラー出力に表示する。標準出力は先に書き出しておく。
void orz_rt_eput(i64 v, const char *desc, i64 newline) {
    orz_rt_flush();
    out_fd = 2;
    put_value(desc, &v, 0);
    if (newline) out("\n", 1);
    orz_rt_flush();
    out_fd = 1;
}

// ---------------- 複製 ----------------

void *orz_rt_copy(const void *p, i64 size) {
    void *q = alloc_obj(size, 0, 0);
    memcpy(q, p, size);
    return q;
}

void *orz_rt_array_copy(const char *a, i64 elem_size, i64 scan) {
    i64 n = ((const i64 *)a)[-1];
    void *b = orz_rt_array_new(n, elem_size, 0, scan, 0);
    memcpy(b, a, n * elem_size);
    return b;
}

// ---------------- 入力とファイル ----------------

static i64 last_errno;
// last_errno がこの値なら、理由は error_text にある（orz で書いた標準ライブラリが決めた文）。
#define ERROR_TEXT (-1)
static char error_text[256];

// 伸びるバッファ。古いものは捨てる（GC が入るまでは解放しない）。
struct buf {
    char *p;
    i64 len, cap;
};

static void buf_push(struct buf *b, const char *p, i64 n) {
    if (b->len + n > b->cap) {
        i64 cap = b->cap ? b->cap * 2 : 256;
        while (cap < b->len + n) cap *= 2;
        char *q = alloc_obj(cap, 1, 0);
        memcpy(q, b->p, b->len);
        b->p = q;
        b->cap = cap;
    }
    memcpy(b->p + b->len, p, n);
    b->len += n;
}

static char inbuf[4096];
static i64 inpos, inlen;
static int ineof;

// 1 行読む。末尾の改行（\n と \r\n）は除く。入力の終わりなら null。
char *orz_rt_read_line(void) {
    orz_rt_flush();
    struct buf line = {0, 0, 0};
    int got = 0;
    for (;;) {
        if (inpos == inlen) {
            if (ineof) break;
            i64 r = os_read(0, inbuf, sizeof inbuf);
            if (r <= 0) {
                ineof = 1;
                break;
            }
            inpos = 0;
            inlen = r;
        }
        got = 1;
        char *start = inbuf + inpos, *end = inbuf + inlen, *nl = start;
        while (nl < end && *nl != '\n') nl++;
        buf_push(&line, start, nl - start);
        if (nl < end) {
            inpos = nl - inbuf + 1;
            if (line.len > 0 && line.p[line.len - 1] == '\r') line.len--;
            return str_from(line.p, line.len);
        }
        inpos = inlen;
    }
    if (!got) return 0;
    return str_from(line.p, line.len);
}

char *orz_rt_read_file(const char *path) {
    i64 fd = os_open_read(path);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    struct buf b = {0, 0, 0};
    char tmp[4096];
    for (;;) {
        i64 r = os_read(fd, tmp, sizeof tmp);
        if (r < 0) {
            last_errno = -r;
            os_close(fd);
            return 0;
        }
        if (r == 0) break;
        buf_push(&b, tmp, r);
    }
    os_close(fd);
    return str_from(b.p, b.len);
}

i64 orz_rt_write_file(const char *path, const char *text) {
    i64 fd = os_open_write(path);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    const char *p = text;
    i64 n = str_len(text);
    while (n > 0) {
        i64 r = os_write(fd, p, n);
        if (r < 0) {
            last_errno = -r;
            os_close(fd);
            return 0;
        }
        p += r;
        n -= r;
    }
    i64 r = os_close(fd);
    if (r < 0) {
        last_errno = -r;
        return 0;
    }
    return 1;
}

// 直前の失敗の理由。
char *orz_rt_last_error(void) {
    const char *m;
    switch (last_errno) {
    case 0:
        return str_empty.s;
    case ERROR_TEXT:
        m = error_text;
        break;
    case 2:
        m = "ファイルやフォルダがありません";
        break;
    case 13:
        m = "権限がありません";
        break;
    case 17:
        m = "すでにあります";
        break;
    case 20:
        m = "パスの途中がフォルダではありません";
        break;
    case 21:
        m = "フォルダです（ファイルではありません）";
        break;
    case 24:
        m = "開いているファイルが多すぎます";
        break;
    case 28:
        m = "ディスクの空きがありません";
        break;
    case 30:
        m = "読み取り専用の場所です";
        break;
    case 36:
        m = "パスが長すぎます";
        break;
    case 8:
        m = "実行できる形式ではありません";
        break;
    case 22:
        m = "値が正しくありません";
        break;
    case 32:
        m = "相手が接続を閉じました";
        break;
    case 98:
        m = "そのアドレス（ポート）は、すでに使われています";
        break;
    case 99:
        m = "そのアドレスは、このコンピュータでは使えません";
        break;
    case 101:
    case 113:
        m = "相手に届く経路がありません";
        break;
    case 104:
        m = "相手に接続を切られました";
        break;
    case 110:
        m = "時間切れです";
        break;
    case 111:
        m = "接続を断られました（相手が待ち受けていません）";
        break;
    default: {
        int win = last_errno >= WIN_ERROR_BASE;
        char a[24], *num = fmt_long(a, win ? last_errno - WIN_ERROR_BASE : last_errno);
        char msg[64] = "エラー（番号 ";
        if (win) memcpy(msg, "エラー（Windows の番号 ", sizeof "エラー（Windows の番号 ");
        i64 l = cstrlen(msg), k = 0;
        while (num[k]) msg[l++] = num[k++];
        const char *tail = "）";
        for (k = 0; tail[k]; k++) msg[l++] = tail[k];
        return str_from(msg, l);
    }
    }
    return str_from(m, cstrlen(m));
}

// ---------------- 累乗 ----------------

// 整数の累乗。bits は 8・32・64。途中の 2 乗が溢れるなら、最後の結果も溢れる。
i64 orz_rt_pow(i64 base, i64 exp, i64 bits, const char *site, const char *overflow, const char *negexp) {
    if (exp < 0) orz_rt_panic(site, negexp);
    i64 lo, hi;
    if (bits == 8) {
        lo = 0;
        hi = 255;
    } else if (bits == 32) {
        lo = -2147483647LL - 1;
        hi = 2147483647LL;
    } else {
        lo = (i64)(1ULL << 63);
        hi = ~lo;
    }
    i64 r = 1;
    while (exp > 0) {
        if (exp & 1) {
            if (__builtin_mul_overflow(r, base, &r) || r < lo || r > hi) orz_rt_panic(site, overflow);
        }
        exp >>= 1;
        if (exp > 0) {
            if (__builtin_mul_overflow(base, base, &base) || base < lo || base > hi) orz_rt_panic(site, overflow);
        }
    }
    return r;
}

// ---------------- プロセス（標準ライブラリ os などから extern で呼ぶ） ----------------

static i64 g_argc;
static char **g_argv;

// p から n バイトの文字列を作る（標準ライブラリ strings で使う）。
char *orz_rt_str_from_bytes(const char *p, i64 n) { return str_from(p, n); }

// 失敗の理由（Linux の errno の番号）を覚える。orz で書いた標準ライブラリから呼ぶ。stdio.last_error() で取れる。
void orz_rt_set_error(i64 code) { last_errno = code; }

// 失敗の理由を、文で覚える（255 バイトまで）。
void orz_rt_set_error_text(const char *msg) {
    i64 n = str_len(msg);
    if (n > (i64)sizeof error_text - 1) n = sizeof error_text - 1;
    memcpy(error_text, msg, n);
    error_text[n] = 0;
    last_errno = ERROR_TEXT;
}

// 実行時エラーで止まる（標準ライブラリで、範囲外などを知らせる）。
__attribute__((noreturn)) void orz_rt_fail(const char *msg) {
    panic_begin(0);
    err(msg);
    panic_end();
}

#ifndef _WIN32

static char **g_envp; // 環境変数（起動時のスタックの、引数の後ろにある）

// 環境変数。なければ null。
char *orz_rt_env(const char *name) {
    i64 n = str_len(name);
    for (char **e = g_envp; e && *e; e++) {
        const char *v = *e;
        if (memcmp(v, name, n) == 0 && v[n] == '=') return str_from(v + n + 1, cstrlen(v + n + 1));
    }
    return 0;
}

i64 orz_rt_remove(const char *path) {
    i64 r = sys3(SYS_unlink, (i64)path, 0, 0);
    if (r < 0) {
        last_errno = -r;
        return 0;
    }
    return 1;
}

// 子のプロセスで、PATH から探して実行する。失敗したら errno を fd に書いて終わる。
__attribute__((noreturn)) static void exec_child(char **argv, int fd) {
    static char path[4096];
    const char *cmd = argv[0];
    i64 err_no = ENOENT;
    int has_slash = 0;
    for (const char *c = cmd; *c; c++)
        if (*c == '/') has_slash = 1;
    if (has_slash) {
        err_no = -sys3(SYS_execve, (i64)cmd, (i64)argv, (i64)g_envp);
    } else {
        const char *dirs = "/usr/local/bin:/usr/bin:/bin";
        for (char **e = g_envp; e && *e; e++)
            if (memcmp(*e, "PATH=", 5) == 0) dirs = *e + 5;
        i64 clen = cstrlen(cmd);
        while (*dirs) {
            i64 d = 0;
            while (dirs[d] && dirs[d] != ':') d++;
            if (d + 1 + clen + 1 <= (i64)sizeof path) {
                i64 k = 0;
                if (d == 0) path[k++] = '.';
                memcpy(path + k, dirs, d);
                k += d;
                path[k++] = '/';
                memcpy(path + k, cmd, clen + 1);
                i64 r = -sys3(SYS_execve, (i64)path, (i64)argv, (i64)g_envp);
                // 見つからない以外の理由（権限など）を覚えておく。
                if (r != ENOENT && r != ENOTDIR) err_no = r;
            }
            dirs += d;
            if (*dirs == ':') dirs++;
        }
    }
    int e32 = (int)err_no;
    sys3(SYS_write, fd, (i64)&e32, 4);
    for (;;) sys3(SYS_exit_group, 127, 0, 0);
}

// コマンドを実行して、終わるまで待つ。終了コード（シグナルで止まったら 128 + 番号）を返す。
// 実行できなければ -1（理由は last_error）。
i64 orz_rt_run(char **args) {
    i64 n = ((const i64 *)args)[-1];
    if (n == 0) {
        last_errno = ENOENT;
        return -1;
    }
    char **argv = map((n + 1) * 8, 0);
    for (i64 i = 0; i < n; i++) argv[i] = args[i];
    argv[n] = 0;
    orz_rt_flush();
    int fds[2];
    i64 r = sys3(SYS_pipe2, (i64)fds, O_CLOEXEC, 0);
    if (r < 0) {
        last_errno = -r;
        unmap(argv, (n + 1) * 8);
        return -1;
    }
    i64 pid = sys3(SYS_fork, 0, 0, 0);
    if (pid == 0) {
        sys3(SYS_close, fds[0], 0, 0);
        exec_child(argv, fds[1]);
    }
    sys3(SYS_close, fds[1], 0, 0);
    unmap(argv, (n + 1) * 8);
    if (pid < 0) {
        sys3(SYS_close, fds[0], 0, 0);
        last_errno = -pid;
        return -1;
    }
    // 実行できたら、書き込み側が閉じて 0 バイトになる。
    int child_err = 0;
    i64 got = os_read(fds[0], (char *)&child_err, 4);
    sys3(SYS_close, fds[0], 0, 0);
    int status = 0;
    while (sys4(SYS_wait4, pid, (i64)&status, 0, 0) == -EINTR) {
    }
    if (got == 4) {
        last_errno = child_err;
        return -1;
    }
    if ((status & 0x7f) == 0) return (status >> 8) & 0xff;
    return 128 + (status & 0x7f);
}

#else // _WIN32

static u16 wname[32768];

char *orz_rt_env(const char *name) {
    i64 n = str_len(name);
    int w = n == 0 ? 0 : MultiByteToWideChar(CP_UTF8, 0, name, (int)n, wname, 32767);
    if (w <= 0) return 0;
    wname[w] = 0;
    u32 len = GetEnvironmentVariableW(wname, wpath, 32768);
    if (len == 0 || len >= 32768) return 0;
    int bytes = WideCharToMultiByte(CP_UTF8, 0, wpath, (int)len, 0, 0, 0, 0);
    char *s = new_str(bytes);
    WideCharToMultiByte(CP_UTF8, 0, wpath, (int)len, s, bytes, 0, 0);
    return s;
}

i64 orz_rt_remove(const char *path) {
    if (!to_wide(path)) {
        last_errno = ENAMETOOLONG;
        return 0;
    }
    if (!DeleteFileW(wpath)) {
        last_errno = -open_error();
        return 0;
    }
    return 1;
}

// 引数を、Windows の決まり（parse_command_line の逆）で 1 つのコマンドラインにする。
static void quote_arg(struct buf *b, const char *a) {
    i64 n = str_len(a);
    int plain = n > 0;
    for (i64 i = 0; i < n; i++)
        if (a[i] == ' ' || a[i] == '\t' || a[i] == '"') plain = 0;
    if (plain) {
        buf_push(b, a, n);
        return;
    }
    buf_push(b, "\"", 1);
    for (i64 i = 0; i < n; i++) {
        i64 k = 0;
        while (i < n && a[i] == '\\') i++, k++;
        if (i == n) {
            // 終わりの「"」の前の「\」は 2 倍にする。
            for (i64 j = 0; j < 2 * k; j++) buf_push(b, "\\", 1);
            break;
        }
        if (a[i] == '"') {
            for (i64 j = 0; j < 2 * k + 1; j++) buf_push(b, "\\", 1);
        } else {
            for (i64 j = 0; j < k; j++) buf_push(b, "\\", 1);
        }
        buf_push(b, a + i, 1);
    }
    buf_push(b, "\"", 1);
}

// STARTUPINFOW と PROCESS_INFORMATION
struct startup_info {
    u32 cb;
    void *reserved, *desktop, *title;
    u32 x, y, xsize, ysize, xcount, ycount, fill, flags;
    unsigned short show, reserved2_len;
    void *reserved2;
    HANDLE in, out, err;
};

struct process_info {
    HANDLE process, thread;
    u32 pid, tid;
};

_Static_assert(sizeof(struct startup_info) == 104, "STARTUPINFOW の大きさ");
_Static_assert(sizeof(struct process_info) == 24, "PROCESS_INFORMATION の大きさ");

i64 orz_rt_run(char **args) {
    i64 n = ((const i64 *)args)[-1];
    if (n == 0) {
        last_errno = ENOENT;
        return -1;
    }
    struct buf b = {0, 0, 0};
    for (i64 i = 0; i < n; i++) {
        if (i > 0) buf_push(&b, " ", 1);
        quote_arg(&b, args[i]);
    }
    u16 *cmd = map((b.len + 1) * 2, 0);
    int w = b.len == 0 ? 0 : MultiByteToWideChar(CP_UTF8, 0, b.p, (int)b.len, cmd, (int)b.len);
    cmd[w] = 0;
    orz_rt_flush();
    // 標準入出力を、そのまま子に渡す（STARTF_USESTDHANDLES）。
    struct startup_info si;
    struct process_info pi;
    memset(&si, 0, sizeof si);
    si.cb = sizeof si;
    si.flags = 0x100;
    HANDLE *std[3] = {&si.in, &si.out, &si.err};
    for (int k = 0; k < 3; k++) {
        *std[k] = os_handle(k);
        SetHandleInformation(*std[k], 1, 1); // HANDLE_FLAG_INHERIT
    }
    int ok = CreateProcessW(0, cmd, 0, 0, 1, 0, 0, 0, &si, &pi);
    unmap(cmd, (b.len + 1) * 2);
    if (!ok) {
        last_errno = win_errno(GetLastError());
        return -1;
    }
    WaitForSingleObject(pi.process, 0xffffffffu);
    u32 code = 0;
    GetExitCodeProcess(pi.process, &code);
    CloseHandle(pi.process);
    CloseHandle(pi.thread);
    return code & 255;
}

#endif // _WIN32

// ---------------- 起動 ----------------

// コマンドライン引数を array:string にする。
void *orz_rt_args(void) {
    char **a = orz_rt_array_new(g_argc, 8, 0, 1, 0);
    for (i64 i = 0; i < g_argc; i++) a[i] = str_from(g_argv[i], cstrlen(g_argv[i]));
    return a;
}

static const char crash_message[] = "実行時エラー: メモリの読み書きに失敗しました"
                                    "（再帰が深すぎてスタックが溢れたか、ポインタが不正な場所を指しています）\n";

#ifndef _WIN32

// スタックが溢れたとき（SIGSEGV）に、メッセージを出して止まる。
// 溢れたスタックでは何もできないので、別のスタック（sigaltstack）で受ける。
static void on_segv(int sig) {
    (void)sig;
    orz_rt_flush();
    err(crash_message);
    os_exit(PANIC_EXIT_CODE);
}

// シグナルから戻るとき用（on_segv は戻らないので、実際には使われない）。
__asm__(".text\n"
        "orz_rt_sigreturn:\n"
        "\tmovl $15, %eax\n"
        "\tsyscall\n");
void orz_rt_sigreturn(void);

static char altstack[65536] __attribute__((aligned(16)));

struct kstack {
    void *sp;
    int flags;
    i64 size;
};

struct ksigaction {
    void (*handler)(int);
    u64 flags;
    void (*restorer)(void);
    u64 mask;
};

// sp は起動時のスタック（argc、argv… が並んでいる）。
static void os_start(i64 *sp) {
    g_argc = sp[0];
    g_argv = (char **)(sp + 1);
    g_envp = g_argv + g_argc + 1;
    stack_top = (char *)sp;
    static struct kstack ss = {altstack, 0, sizeof altstack};
    static struct ksigaction sa = {on_segv, 0x04000000 /* SA_RESTORER */ | 0x08000000 /* SA_ONSTACK */,
                                   orz_rt_sigreturn, 0};
    sys3(SYS_sigaltstack, (i64)&ss, 0, 0);
    sys4(SYS_rt_sigaction, 11 /* SIGSEGV */, (i64)&sa, 0, 8);
    sys4(SYS_rt_sigaction, 7 /* SIGBUS */, (i64)&sa, 0, 8);
}

#else // _WIN32

// メモリの読み書きの失敗とスタックの溢れを受けて、メッセージを出して止まる。
static i64 WINAPI on_exception(void *info) {
    u32 code = **(u32 **)info;
    // EXCEPTION_ACCESS_VIOLATION、EXCEPTION_IN_PAGE_ERROR、EXCEPTION_STACK_OVERFLOW
    if (code != 0xc0000005u && code != 0xc0000006u && code != 0xc00000fdu) return 0;
    orz_rt_flush();
    err(crash_message);
    os_exit(PANIC_EXIT_CODE);
}

// コマンドライン（UTF-16）を、Windows の決まり（MSVC の C ランタイムと同じ）で引数に分け、UTF-8 にする。
static void parse_command_line(void) {
    const u16 *cmd = GetCommandLineW();
    i64 len = 0;
    while (cmd[len]) len++;
    u16 *w = map((len + 1) * 2, 0);
    char *out = map(len * 3 + len + 16, 0), *o = out;
    char **argv = map((len + 2) * 8, 0);
    i64 argc = 0, i = 0;
    for (;;) {
        i64 n = 0;
        if (argc == 0) {
            // プログラムの名前：「"」で囲まれていればその中、なければ空白まで。「\」は特別ではない。
            if (cmd[i] == '"') {
                for (i++; cmd[i] && cmd[i] != '"'; i++) w[n++] = cmd[i];
                if (cmd[i]) i++;
            } else {
                for (; cmd[i] && cmd[i] != ' ' && cmd[i] != '\t'; i++) w[n++] = cmd[i];
            }
        } else {
            while (cmd[i] == ' ' || cmd[i] == '\t') i++;
            if (!cmd[i]) break;
            int quoted = 0;
            while (cmd[i] && (quoted || (cmd[i] != ' ' && cmd[i] != '\t'))) {
                if (cmd[i] == '\\') {
                    i64 k = 0;
                    while (cmd[i] == '\\') i++, k++;
                    if (cmd[i] == '"') {
                        // 「"」の前の「\」は 2 つで 1 つ。奇数なら「"」はそのままの文字。
                        for (i64 j = 0; j < k / 2; j++) w[n++] = '\\';
                        if (k % 2) w[n++] = cmd[i++];
                    } else {
                        for (i64 j = 0; j < k; j++) w[n++] = '\\';
                    }
                } else if (cmd[i] == '"') {
                    if (quoted && cmd[i + 1] == '"') {
                        w[n++] = '"';
                        i += 2;
                    } else {
                        quoted = !quoted;
                        i++;
                    }
                } else {
                    w[n++] = cmd[i++];
                }
            }
        }
        argv[argc++] = o;
        if (n > 0) o += WideCharToMultiByte(CP_UTF8, 0, w, (int)n, o, (int)(out + len * 3 + len + 16 - o), 0, 0);
        *o++ = 0;
        if (argc == 1 && !cmd[i]) break;
    }
    g_argc = argc;
    g_argv = argv;
}

static void os_start(i64 *sp) {
    (void)sp;
    // スタックの底（GC が調べる範囲の終わり）は、TEB（%gs）の StackBase にある。
    __asm__ volatile("movq %%gs:8, %0" : "=r"(stack_top));
    // スタックが溢れたときに、メッセージを出すだけの余裕を残しておく。
    u32 guarantee = 65536;
    SetThreadStackGuarantee(&guarantee);
    AddVectoredExceptionHandler(1, on_exception);
    parse_command_line();
}

#endif // _WIN32

// コンパイラが出す。main を呼んで、終了コードを返す。
i64 orz_entry(void);

// _start から呼ばれる。sp は起動時のスタック。
__attribute__((noreturn)) void orz_rt_start(i64 *sp) {
    os_start(sp);
    i64 code = orz_entry();
    // arena の出入りは対になっているはず（コンパイラの誤りを見つけるため）。
    if (narenas != 0) orz_rt_panic(0, "（コンパイラの誤り）arena の出入りが合っていません");
    orz_rt_exit(code);
}
