// orz のランタイム（x86-64 Linux）。
//
// C ライブラリは使わず、システムコールだけを使う（-ffreestanding でコンパイルする）。
// コンパイラが出したアセンブリから、C と同じ呼び出し方（System V ABI）で呼ばれる。
//
// 値の持ち方：
//   string  先頭のバイトを指すアドレス。長さはその手前の 8 バイトにあり、最後に 0 が付いている
//   array   先頭の要素を指すアドレス。要素数はその手前の 8 バイトにある
//
// メモリは確保するだけで、プログラムの終わりにまとめて解放する（GC は実装の手順 7 で入れる）。

typedef unsigned long usize;

#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_mmap 9
#define SYS_rt_sigaction 13
#define SYS_sigaltstack 131
#define SYS_exit_group 231

#define O_RDONLY 0
#define O_WRONLY 1
#define O_CREAT 0100
#define O_TRUNC 01000
#define O_CLOEXEC 02000000

#define EINTR 4
#define PANIC_EXIT_CODE 101

static long sys3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static long sys4(long n, long a, long b, long c, long d) {
    long r;
    register long r10 __asm__("r10") = d;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10) : "rcx", "r11", "memory");
    return r;
}

static long sys6(long n, long a, long b, long c, long d, long e, long f) {
    long r;
    register long r10 __asm__("r10") = d;
    register long r8 __asm__("r8") = e;
    register long r9 __asm__("r9") = f;
    __asm__ volatile("syscall"
                     : "=a"(r)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return r;
}

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

static long cstrlen(const char *s) {
    long n = 0;
    while (s[n]) n++;
    return n;
}

// ---------------- 長さの付いた文字列 ----------------

#define STATIC_STR(name, lit) \
    static struct { long len; char s[sizeof(lit)]; } __attribute__((aligned(8))) name = {sizeof(lit) - 1, lit}

STATIC_STR(str_empty, "");
STATIC_STR(str_true, "true");
STATIC_STR(str_false, "false");

static struct { long len; long data[1]; } __attribute__((aligned(8))) empty_array = {0, {0}};

static long str_len(const char *s) { return ((const long *)s)[-1]; }

// ---------------- 出力（バッファ付き） ----------------

static char outbuf[8192];
static long outlen;

static void write_all(int fd, const char *p, long n) {
    while (n > 0) {
        long r = sys3(SYS_write, fd, (long)p, n);
        if (r == -EINTR) continue;
        if (r <= 0) return;
        p += r;
        n -= r;
    }
}

void orz_rt_flush(void) {
    write_all(1, outbuf, outlen);
    outlen = 0;
}

static void out(const char *p, long n) {
    if (n > (long)sizeof outbuf - outlen) {
        orz_rt_flush();
        if (n >= (long)sizeof outbuf) {
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
static char *fmt_long(char *buf, long v) {
    char *p = buf + 21;
    *--p = 0;
    unsigned long u = v < 0 ? -(unsigned long)v : (unsigned long)v;
    do {
        *--p = (char)('0' + u % 10);
        u /= 10;
    } while (u);
    if (v < 0) *--p = '-';
    return p;
}

static char *fmt_hex(char *buf, unsigned long v) {
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

__attribute__((noreturn)) static void sys_exit(long code) {
    for (;;) sys3(SYS_exit_group, code, 0, 0);
}

__attribute__((noreturn)) void orz_rt_exit(long code) {
    orz_rt_flush();
    sys_exit(code);
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
    sys_exit(PANIC_EXIT_CODE);
}

__attribute__((noreturn)) void orz_rt_panic(const char *site, const char *reason) {
    panic_begin(site);
    err(reason);
    panic_end();
}

__attribute__((noreturn)) void orz_rt_panic_index(const char *site, long index, long len) {
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
__attribute__((noreturn)) void orz_rt_panic_convert(const char *site, long value, const char *reason) {
    char a[24];
    panic_begin(site);
    err("変換する値 ");
    err(fmt_long(a, value));
    err(" が ");
    err(reason);
    panic_end();
}

// ---------------- メモリ ----------------

#define CHUNK (4L << 20)

static char *heap_cur, *heap_end;

static void *map(long n, const char *site) {
    long p = sys6(SYS_mmap, 0, n, 3 /* 読み書き */, 0x22 /* MAP_PRIVATE | MAP_ANONYMOUS */, -1, 0);
    if (p < 0 && p > -4096) orz_rt_panic(site, "メモリが足りません");
    return (void *)p;
}

// n バイトを確保する。中身は 0。
void *orz_rt_alloc(long n, const char *site) {
    if (n < 0) {
        char a[24];
        panic_begin(site);
        err("allocate に負の大きさ ");
        err(fmt_long(a, n));
        err(" が渡されました");
        panic_end();
    }
    if (n > (1L << 46)) orz_rt_panic(site, "メモリが足りません");
    n = (n + 15) & ~15L;
    if (n == 0) n = 16;
    if (n > heap_end - heap_cur) {
        if (n >= CHUNK / 4) return map(n, site);
        heap_cur = map(CHUNK, site);
        heap_end = heap_cur + CHUNK;
    }
    void *r = heap_cur;
    heap_cur += n;
    return r;
}

static char *new_str(long len) {
    long *h = orz_rt_alloc(8 + len + 1, 0);
    h[0] = len;
    return (char *)(h + 1);
}

static char *str_from(const char *p, long len) {
    char *s = new_str(len);
    memcpy(s, p, len);
    return s;
}

// 配列を作る。kind：0 = 中身は 0、1 = 空の文字列、2 = 空の配列。
void *orz_rt_array_new(long count, long elem_size, long kind, const char *site) {
    if (count < 0) {
        char a[24];
        panic_begin(site);
        err("配列の長さ ");
        err(fmt_long(a, count));
        err(" が負です");
        panic_end();
    }
    if (count > (1L << 40) / elem_size) orz_rt_panic(site, "メモリが足りません");
    long *h = orz_rt_alloc(8 + count * elem_size, site);
    h[0] = count;
    void **data = (void **)(h + 1);
    if (kind == 1)
        for (long i = 0; i < count; i++) data[i] = str_empty.s;
    else if (kind == 2)
        for (long i = 0; i < count; i++) data[i] = empty_array.data;
    return data;
}

// ---------------- 文字列 ----------------

char *orz_rt_str_concat(const char *a, const char *b) {
    long la = str_len(a), lb = str_len(b);
    char *s = new_str(la + lb);
    memcpy(s, a, la);
    memcpy(s + la, b, lb);
    return s;
}

// 中身が同じか。どちらかが null（0）のときは、両方 null なら同じ。
long orz_rt_str_eq(const char *a, const char *b) {
    if (a == b) return 1;
    if (!a || !b) return 0;
    long la = str_len(a);
    return la == str_len(b) && memcmp(a, b, la) == 0;
}

char *orz_rt_str_from_int(long v) {
    char buf[24];
    char *p = fmt_long(buf, v);
    return str_from(p, cstrlen(p));
}

char *orz_rt_str_from_bool(long v) { return v ? str_true.s : str_false.s; }

// ---------------- 大きな整数（decnum の表示と読み取りに使う） ----------------

#define BIG_WORDS 160

typedef struct {
    int n; // 使っている語の数（上の語は 0 でない）
    unsigned d[BIG_WORDS];
} big;

static void big_set(big *a, unsigned long v) {
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
    unsigned long carry = 0;
    for (int i = 0; i < a->n; i++) {
        unsigned long t = (unsigned long)a->d[i] * m + carry;
        a->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    if (carry) a->d[a->n++] = (unsigned)carry;
}

static void big_add_small(big *a, unsigned v) {
    unsigned long carry = v;
    for (int i = 0; i < a->n && carry; i++) {
        unsigned long t = (unsigned long)a->d[i] + carry;
        a->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    if (carry) a->d[a->n++] = (unsigned)carry;
}

// r = a + b（r は a と同じでもよい）
static void big_add(big *r, const big *a, const big *b) {
    int n = a->n > b->n ? a->n : b->n;
    unsigned long carry = 0;
    for (int i = 0; i < n; i++) {
        unsigned long t = carry + (i < a->n ? a->d[i] : 0) + (i < b->n ? b->d[i] : 0);
        r->d[i] = (unsigned)t;
        carry = t >> 32;
    }
    r->n = n;
    if (carry) r->d[r->n++] = (unsigned)carry;
}

// a -= b（a >= b）
static void big_sub(big *a, const big *b) {
    long borrow = 0;
    for (int i = 0; i < a->n; i++) {
        long t = (long)a->d[i] - (i < b->n ? (long)b->d[i] : 0) - borrow;
        borrow = t < 0;
        if (t < 0) t += 1L << 32;
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
static unsigned long big_div(big *n, const big *d, int *rem_nonzero) {
    unsigned long q = 0;
    int shift = big_bitlen(n) - big_bitlen(d);
    big t;
    for (int i = shift; i >= 0; i--) {
        t = *d;
        big_shl(&t, i);
        if (big_cmp(n, &t) >= 0) {
            big_sub(n, &t);
            q |= 1UL << i;
        }
    }
    *rem_nonzero = n->n != 0;
    return q;
}

// ---------------- decnum ----------------

static double from_bits(long v) {
    union {
        long l;
        double d;
    } u = {v};
    return u.d;
}

static long to_bits(double d) {
    union {
        double d;
        long l;
    } u = {d};
    return u.l;
}

// 元の値に戻せるいちばん短い 10 進の桁を作る（Burger & Dybvig の方法）。
// 値 = f × 2^e（f > 0）。値 = 0.d1d2… × 10^k となる k を返し、桁は buf に入る。
static int dec_digits(unsigned long f, int e, char *buf, int *k_out) {
    big r, s, mp, mm, t;
    int even = (f % 2 == 0); // 偶数なら、ちょうど境目も元の値に戻る
    if (e >= 0) {
        if (f != (1UL << 52)) {
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
        if (e == -1074 || f != (1UL << 52)) {
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
    unsigned long bits = (unsigned long)to_bits(x);
    char *p = out;
    if (bits >> 63) *p++ = '-';
    bits &= ~(1UL << 63);
    if (bits == 0) {
        memcpy(p, "0.0", 4);
        return;
    }
    int be = (int)(bits >> 52);
    unsigned long f = bits & ((1UL << 52) - 1);
    int e;
    if (be == 0) {
        e = -1074;
    } else {
        f |= 1UL << 52;
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
static int parse_dec(const char *s, long len, double *out) {
    long i = 0;
    int neg = 0;
    if (i < len && (s[i] == '+' || s[i] == '-')) neg = s[i++] == '-';
    big num, den;
    big_set(&num, 0);
    int ndig = 0, any = 0, sticky = 0;
    long exp10 = 0; // 値 = num × 10^exp10
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
        long ev = 0;
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
    long mag = ndig + exp10; // 値 < 10^mag
    if (mag > 310) return 0;
    if (mag < -330) {
        *out = neg ? -0.0 : 0.0;
        return 1;
    }
    big_set(&den, 1);
    for (long k = 0; k < exp10; k++) big_mul_small(&num, 10);
    for (long k = 0; k < -exp10; k++) big_mul_small(&den, 10);
    // q = floor(num × 2^sh / den) が 2^53 以上 2^54 未満になる sh を探す。
    int sh = 54 - (big_bitlen(&num) - big_bitlen(&den));
    unsigned long q;
    int rem;
    for (;;) {
        big n = num, d = den;
        if (sh >= 0)
            big_shl(&n, sh);
        else
            big_shl(&d, -sh);
        q = big_div(&n, &d, &rem);
        if (q >= (1UL << 54)) {
            sh--;
            continue;
        }
        if (q < (1UL << 53)) {
            sh++;
            continue;
        }
        break;
    }
    long e2 = 1 - sh; // 値 ≒ (q / 2) × 2^e2
    unsigned long bits;
    if (e2 < -1074) {
        // 非正規化数：2^-1074 の単位で丸め直す。
        big n = num, d = den;
        big_shl(&n, 1075);
        q = big_div(&n, &d, &rem);
        unsigned long m = q >> 1;
        if ((q & 1) && (rem || (m & 1))) m++;
        bits = m;
    } else {
        unsigned long m = q >> 1;
        if ((q & 1) && (rem || (m & 1))) m++;
        if (m == (1UL << 53)) {
            m >>= 1;
            e2++;
        }
        if (e2 > 971) return 0;
        bits = ((unsigned long)(e2 + 1075) << 52) | (m - (1UL << 52));
    }
    if (neg) bits |= 1UL << 63;
    *out = from_bits((long)bits);
    return 1;
}

char *orz_rt_str_from_dec(long v) {
    char buf[48];
    fmt_dec(buf, from_bits(v));
    return str_from(buf, cstrlen(buf));
}

void *orz_rt_box(long v) {
    long *p = orz_rt_alloc(8, 0);
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
void *orz_rt_str_to_int(const char *s, long bits) {
    long len = str_len(s), i = 0;
    int neg = 0;
    if (i < len && (s[i] == '+' || s[i] == '-')) neg = s[i++] == '-';
    if (i >= len) return 0;
    unsigned long v = 0, limit = neg ? (1UL << 63) : (1UL << 63) - 1;
    for (; i < len; i++) {
        if (s[i] < '0' || s[i] > '9') return 0;
        unsigned d = (unsigned)(s[i] - '0');
        if (v > (limit - d) / 10) return 0;
        v = v * 10 + d;
    }
    long x = neg ? (long)(0 - v) : (long)v;
    if (bits == 8 && (x < 0 || x > 255)) return 0;
    if (bits == 32 && (x < -2147483647L - 1 || x > 2147483647L)) return 0;
    return orz_rt_box(x);
}

// x87 の fprem で、誤差なしの余りを出す（C の fmod と同じ。符号は割られる数と同じ）。
long orz_rt_fmod(long a, long b) {
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
    return a >= 4503599627370496.0 || (double)(long)y == y;
}

static int is_odd_integer(double y) {
    double a = y < 0 ? -y : y;
    return a < 9007199254740992.0 && (double)(long)y == y && ((long)y & 1);
}

static const char dec_overflow[] = "計算の結果が decnum の範囲を超えました";

// decnum の累乗。
long orz_rt_fpow(long a, long b, const char *site) {
    double x = from_bits(a), y = from_bits(b), r;
    if (y == 0) {
        r = 1.0;
    } else if (x == 0) {
        if (y < 0) orz_rt_panic(site, "0 を負の数で累乗しました");
        r = is_odd_integer(y) ? x : 0.0;
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
            unsigned long n = (unsigned long)ay;
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
    unsigned long bits = (unsigned long)to_bits(r);
    if (((bits >> 52) & 0x7ff) == 0x7ff) orz_rt_panic(site, dec_overflow);
    return to_bits(r);
}

__attribute__((noreturn)) void orz_rt_panic_convert_dec(const char *site, long value, const char *reason) {
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

static void put_int(long v) {
    char buf[24];
    outs(fmt_long(buf, v));
}

static void put_addr(long v) {
    char buf[24];
    outs(fmt_hex(buf, (unsigned long)v));
}

static void put_str(const char *s) {
    if (!s)
        outs("null");
    else
        out(s, str_len(s));
}

void orz_rt_newline(void) { out("\n", 1); }

// 構造体を表示するための説明。コンパイラが出す。
extern const char *const orz_struct_descs[];

static long elem_size(const char *desc) {
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
        put_int(*(const long *)p);
        break;
    case 'd': {
        char buf[48];
        fmt_dec(buf, from_bits(*(const long *)p));
        outs(buf);
        break;
    }
    case 'p':
        put_addr(*(const long *)p);
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
        long n = ((const long *)a)[-1], size = elem_size(desc + 1);
        out("[", 1);
        for (long i = 0; i < n; i++) {
            if (i) out(", ", 2);
            put_value(desc + 1, a + i * size, 1);
        }
        out("]", 1);
        put_depth--;
        break;
    }
    case 'S': {
        const char *obj = *(const char *const *)p;
        long index = 0;
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
            long offset = 0;
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
void orz_rt_put(long v, const char *desc) { put_value(desc, &v, 0); }

// ---------------- 複製 ----------------

void *orz_rt_copy(const void *p, long size) {
    void *q = orz_rt_alloc(size, 0);
    memcpy(q, p, size);
    return q;
}

void *orz_rt_array_copy(const char *a, long elem_size) {
    long n = ((const long *)a)[-1];
    void *b = orz_rt_array_new(n, elem_size, 0, 0);
    memcpy(b, a, n * elem_size);
    return b;
}

// ---------------- 入力とファイル ----------------

static long last_errno;

// 伸びるバッファ。古いものは捨てる（GC が入るまでは解放しない）。
struct buf {
    char *p;
    long len, cap;
};

static void buf_push(struct buf *b, const char *p, long n) {
    if (b->len + n > b->cap) {
        long cap = b->cap ? b->cap * 2 : 256;
        while (cap < b->len + n) cap *= 2;
        char *q = orz_rt_alloc(cap, 0);
        memcpy(q, b->p, b->len);
        b->p = q;
        b->cap = cap;
    }
    memcpy(b->p + b->len, p, n);
    b->len += n;
}

static char inbuf[4096];
static long inpos, inlen;
static int ineof;

// 1 行読む。末尾の改行（\n と \r\n）は除く。入力の終わりなら null。
char *orz_rt_read_line(void) {
    orz_rt_flush();
    struct buf line = {0, 0, 0};
    int got = 0;
    for (;;) {
        if (inpos == inlen) {
            if (ineof) break;
            long r = sys3(SYS_read, 0, (long)inbuf, sizeof inbuf);
            if (r == -EINTR) continue;
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
    long fd = sys4(SYS_open, (long)path, O_RDONLY | O_CLOEXEC, 0, 0);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    struct buf b = {0, 0, 0};
    char tmp[4096];
    for (;;) {
        long r = sys3(SYS_read, fd, (long)tmp, sizeof tmp);
        if (r == -EINTR) continue;
        if (r < 0) {
            last_errno = -r;
            sys3(SYS_close, fd, 0, 0);
            return 0;
        }
        if (r == 0) break;
        buf_push(&b, tmp, r);
    }
    sys3(SYS_close, fd, 0, 0);
    return str_from(b.p, b.len);
}

long orz_rt_write_file(const char *path, const char *text) {
    long fd = sys4(SYS_open, (long)path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644, 0);
    if (fd < 0) {
        last_errno = -fd;
        return 0;
    }
    const char *p = text;
    long n = str_len(text);
    while (n > 0) {
        long r = sys3(SYS_write, fd, (long)p, n);
        if (r == -EINTR) continue;
        if (r < 0) {
            last_errno = -r;
            sys3(SYS_close, fd, 0, 0);
            return 0;
        }
        p += r;
        n -= r;
    }
    long r = sys3(SYS_close, fd, 0, 0);
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
    default: {
        char a[24], *num = fmt_long(a, last_errno);
        char msg[64] = "エラー（番号 ";
        long l = cstrlen(msg), k = 0;
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
long orz_rt_pow(long base, long exp, long bits, const char *site, const char *overflow, const char *negexp) {
    if (exp < 0) orz_rt_panic(site, negexp);
    long lo, hi;
    if (bits == 8) {
        lo = 0;
        hi = 255;
    } else if (bits == 32) {
        lo = -2147483647L - 1;
        hi = 2147483647L;
    } else {
        lo = (long)(1UL << 63);
        hi = ~lo;
    }
    long r = 1;
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

// ---------------- 起動 ----------------

static long g_argc;
static char **g_argv;

// コマンドライン引数を array:string にする。
void *orz_rt_args(void) {
    char **a = orz_rt_array_new(g_argc, 8, 0, 0);
    for (long i = 0; i < g_argc; i++) a[i] = str_from(g_argv[i], cstrlen(g_argv[i]));
    return a;
}

// スタックが溢れたとき（SIGSEGV）に、メッセージを出して止まる。
// 溢れたスタックでは何もできないので、別のスタック（sigaltstack）で受ける。
static void on_segv(int sig) {
    (void)sig;
    orz_rt_flush();
    err("実行時エラー: メモリの読み書きに失敗しました"
        "（再帰が深すぎてスタックが溢れたか、ポインタが不正な場所を指しています）\n");
    sys_exit(PANIC_EXIT_CODE);
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
    long size;
};

struct ksigaction {
    void (*handler)(int);
    unsigned long flags;
    void (*restorer)(void);
    unsigned long mask;
};

static void setup_signals(void) {
    static struct kstack ss = {altstack, 0, sizeof altstack};
    static struct ksigaction sa = {on_segv, 0x04000000 /* SA_RESTORER */ | 0x08000000 /* SA_ONSTACK */,
                                   orz_rt_sigreturn, 0};
    sys3(SYS_sigaltstack, (long)&ss, 0, 0);
    sys4(SYS_rt_sigaction, 11 /* SIGSEGV */, (long)&sa, 0, 8);
    sys4(SYS_rt_sigaction, 7 /* SIGBUS */, (long)&sa, 0, 8);
}

// コンパイラが出す。main を呼んで、終了コードを返す。
long orz_entry(void);

// _start から呼ばれる。sp は起動時のスタック（argc、argv… が並んでいる）。
__attribute__((noreturn)) void orz_rt_start(long *sp) {
    g_argc = sp[0];
    g_argv = (char **)(sp + 1);
    setup_signals();
    orz_rt_exit(orz_entry());
}
