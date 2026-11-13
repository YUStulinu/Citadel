/* /bin/sh: a small POSIX-flavoured shell.
   Supports: quoting, $VAR ${VAR} ${VAR:-x} $? $$ $# $0-$9 $@, $((arithmetic)), $(command),
   globbing (* ? [...]) in the last path component, pipelines, && || ; &, redirections
   (< > >> n> n>> n>&m <&), if/elif/else, while/until, for, ( subshell ), { group },
   builtins (cd exit export unset set echo pwd true false test [ read source . exec wait help),
   and job control for foreground pipelines (process groups, ^C goes to the job). */
#include <ctype.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int interactive, last_status, shell_pgid;
static int script_argc;
static char **script_argv;
static int exiting, exit_code, returning;

/* ------------------------------------------------------------------ utilities */

static void *xmalloc(size_t n) {
    void *p = malloc(n);
    if (!p) {
        fputs("sh: out of memory\n", stderr);
        exit(2);
    }
    return p;
}

static char *xstrndup(const char *s, size_t n) {
    char *d = xmalloc(n + 1);
    memcpy(d, s, n);
    d[n] = 0;
    return d;
}

typedef struct {
    char *s;
    size_t len, cap;
} Str;

static void str_putn(Str *b, const char *s, size_t n) {
    if (b->len + n + 1 > b->cap) {
        b->cap = (b->len + n + 1) * 2;
        char *ns = xmalloc(b->cap);
        if (b->s) memcpy(ns, b->s, b->len);
        free(b->s);
        b->s = ns;
    }
    memcpy(b->s + b->len, s, n);
    b->len += n;
    b->s[b->len] = 0;
}
static void str_putc(Str *b, char c) { str_putn(b, &c, 1); }
static void str_puts(Str *b, const char *s) { str_putn(b, s, strlen(s)); }

typedef struct {
    char **v;
    int n, cap;
} List;

static void list_push(List *l, char *s) {
    if (l->n + 2 > l->cap) {
        l->cap = l->cap ? l->cap * 2 : 8;
        char **nv = xmalloc(l->cap * sizeof *nv);
        if (l->v) memcpy(nv, l->v, l->n * sizeof *nv);
        free(l->v);
        l->v = nv;
    }
    l->v[l->n++] = s;
    l->v[l->n] = NULL;
}

/* ------------------------------------------------------------------ variables */

typedef struct Var {
    char *name, *value;
    int exported;
    struct Var *next;
} Var;
static Var *vars;

static Var *var_find(const char *name, size_t n) {
    for (Var *v = vars; v; v = v->next)
        if (strlen(v->name) == n && !memcmp(v->name, name, n)) return v;
    return NULL;
}

static const char *var_get(const char *name) {
    Var *v = var_find(name, strlen(name));
    return v ? v->value : NULL;
}

static void var_set(const char *name, const char *value, int export) {
    Var *v = var_find(name, strlen(name));
    if (!v) {
        v = xmalloc(sizeof *v);
        v->name = strdup(name);
        v->value = NULL;
        v->exported = 0;
        v->next = vars;
        vars = v;
    }
    if (value) {
        free(v->value);
        v->value = strdup(value);
    }
    if (export) v->exported = 1;
}

static void var_unset(const char *name) {
    for (Var **pp = &vars; *pp; pp = &(*pp)->next)
        if (!strcmp((*pp)->name, name)) {
            Var *v = *pp;
            *pp = v->next;
            free(v->name);
            free(v->value);
            free(v);
            return;
        }
}

static char **build_env(List *extra) {
    List env = {0};
    for (Var *v = vars; v; v = v->next) {
        if (!v->exported || !v->value) continue;
        int overridden = 0;
        for (int i = 0; extra && i < extra->n; i++) {
            size_t l = strcspn(extra->v[i], "=");
            if (strlen(v->name) == l && !strncmp(v->name, extra->v[i], l)) overridden = 1;
        }
        if (overridden) continue;
        Str s = {0};
        str_puts(&s, v->name);
        str_putc(&s, '=');
        str_puts(&s, v->value);
        list_push(&env, s.s);
    }
    for (int i = 0; extra && i < extra->n; i++) list_push(&env, strdup(extra->v[i]));
    if (!env.v) list_push(&env, NULL), env.n = 0;
    return env.v;
}

static int valid_name(const char *s, size_t n) {
    if (!n || !(isalpha((unsigned char)s[0]) || s[0] == '_')) return 0;
    for (size_t i = 1; i < n; i++)
        if (!(isalnum((unsigned char)s[i]) || s[i] == '_')) return 0;
    return 1;
}

static int is_assignment(const char *w) {
    const char *eq = strchr(w, '=');
    return eq && valid_name(w, eq - w);
}

/* ------------------------------------------------------------------ lexer */

enum { T_WORD, T_OP, T_REDIR, T_NL, T_EOF };
enum { R_IN, R_OUT, R_APPEND, R_DUPOUT, R_DUPIN, R_RDWR };

typedef struct {
    int type;
    char *text; /* word (raw, quotes kept) or operator */
    int fd, kind;
} Token;

typedef struct {
    Token *t;
    int n, cap, pos;
    int incomplete; /* unterminated quote or $( */
} Tokens;

static void tok_push(Tokens *ts, Token t) {
    if (ts->n == ts->cap) {
        ts->cap = ts->cap ? ts->cap * 2 : 64;
        Token *nt = xmalloc(ts->cap * sizeof *nt);
        if (ts->t) memcpy(nt, ts->t, ts->n * sizeof *nt);
        free(ts->t);
        ts->t = nt;
    }
    ts->t[ts->n++] = t;
}

/* Skip a $( ... ) or $(( ... )) group starting at s (pointing at '('); returns the char after ')'. */
static const char *skip_parens(const char *s) {
    int depth = 0;
    char q = 0;
    for (; *s; s++) {
        if (q) {
            if (*s == '\\' && q == '"' && s[1]) s++;
            else if (*s == q) q = 0;
            continue;
        }
        if (*s == '\'' || *s == '"') q = *s;
        else if (*s == '\\' && s[1]) s++;
        else if (*s == '(') depth++;
        else if (*s == ')' && --depth == 0) return s + 1;
    }
    return NULL;
}

static void lex(const char *s, Tokens *ts) {
    ts->n = ts->pos = ts->incomplete = 0;
    while (*s) {
        if (*s == ' ' || *s == '\t' || *s == '\r') {
            s++;
            continue;
        }
        if (*s == '\\' && s[1] == '\n') {
            s += 2;
            continue;
        }
        if (*s == '#') {
            while (*s && *s != '\n') s++;
            continue;
        }
        if (*s == '\n') {
            tok_push(ts, (Token){T_NL, "\n", 0, 0});
            s++;
            continue;
        }
        /* io number: digits immediately followed by < or > */
        const char *d = s;
        while (isdigit((unsigned char)*d)) d++;
        int iofd = -1;
        if (d > s && (*d == '<' || *d == '>')) {
            iofd = atoi(s);
            s = d;
        }
        if (*s == '<' || *s == '>') {
            Token t = {T_REDIR, NULL, 0, 0};
            if (s[0] == '>' && s[1] == '>') t.kind = R_APPEND, s += 2;
            else if (s[0] == '>' && s[1] == '&') t.kind = R_DUPOUT, s += 2;
            else if (s[0] == '<' && s[1] == '&') t.kind = R_DUPIN, s += 2;
            else if (s[0] == '<' && s[1] == '>') t.kind = R_RDWR, s += 2;
            else if (s[0] == '>' && s[1] == '|') t.kind = R_OUT, s += 2;
            else t.kind = *s == '<' ? R_IN : R_OUT, s++;
            t.fd = iofd >= 0 ? iofd : (t.kind == R_IN || t.kind == R_DUPIN || t.kind == R_RDWR) ? 0 : 1;
            tok_push(ts, t);
            continue;
        }
        if (strchr("|&;()", *s)) {
            static const char *const ops[] = {"&&", "||", ";;", "|", "&", ";", "(", ")"};
            for (unsigned i = 0; i < sizeof ops / sizeof *ops; i++) {
                size_t l = strlen(ops[i]);
                if (!strncmp(s, ops[i], l)) {
                    tok_push(ts, (Token){T_OP, (char *)ops[i], 0, 0});
                    s += l;
                    break;
                }
            }
            continue;
        }
        /* a word: runs until unquoted blank or operator */
        const char *start = s;
        while (*s && !strchr(" \t\r\n|&;()<>", *s)) {
            if (*s == '\\' && s[1]) {
                s += 2;
            } else if (*s == '\'') {
                const char *e = strchr(s + 1, '\'');
                if (!e) {
                    ts->incomplete = 1;
                    return;
                }
                s = e + 1;
            } else if (*s == '"') {
                s++;
                while (*s && *s != '"') {
                    if (*s == '\\' && s[1]) s += 2;
                    else if (*s == '$' && s[1] == '(') {
                        const char *e = skip_parens(s + 1);
                        if (!e) {
                            ts->incomplete = 1;
                            return;
                        }
                        s = e;
                    } else s++;
                }
                if (!*s) {
                    ts->incomplete = 1;
                    return;
                }
                s++;
            } else if (*s == '$' && s[1] == '(') {
                const char *e = skip_parens(s + 1);
                if (!e) {
                    ts->incomplete = 1;
                    return;
                }
                s = e;
            } else if (*s == '$' && s[1] == '{') {
                const char *e = strchr(s, '}');
                if (!e) {
                    ts->incomplete = 1;
                    return;
                }
                s = e + 1;
            } else {
                s++;
            }
        }
        tok_push(ts, (Token){T_WORD, xstrndup(start, s - start), 0, 0});
    }
    tok_push(ts, (Token){T_EOF, "", 0, 0});
}

/* ------------------------------------------------------------------ parser */

enum { N_CMD, N_PIPE, N_AND, N_OR, N_SEQ, N_BG, N_NOT, N_IF, N_WHILE, N_UNTIL, N_FOR, N_SUBSHELL, N_GROUP, N_FUNC };

typedef struct Redir {
    int fd, kind;
    char *word;
    struct Redir *next;
} Redir;

typedef struct Node {
    int type;
    struct Node *a, *b, *c;
    List words;
    char *var;
    Redir *redir;
} Node;

static int parse_error, parse_need_more;

static Node *node(int type) {
    Node *n = xmalloc(sizeof *n);
    memset(n, 0, sizeof *n);
    n->type = type;
    return n;
}

static Token *peek(Tokens *ts) { return &ts->t[ts->pos]; }
static int is_word(Tokens *ts, const char *w) {
    Token *t = peek(ts);
    return t->type == T_WORD && !strcmp(t->text, w);
}
static int is_op(Tokens *ts, const char *o) {
    Token *t = peek(ts);
    return t->type == T_OP && !strcmp(t->text, o);
}
static void skip_nl(Tokens *ts) {
    while (peek(ts)->type == T_NL) ts->pos++;
}

static void syntax(Tokens *ts, const char *what) {
    if (parse_error) return;
    if (peek(ts)->type == T_EOF) {
        parse_need_more = 1;
    } else {
        fprintf(stderr, "sh: syntax error near '%s' (%s)\n", peek(ts)->type == T_NL ? "newline" : peek(ts)->text, what);
    }
    parse_error = 1;
}

static int expect_word(Tokens *ts, const char *w) {
    skip_nl(ts);
    if (!is_word(ts, w)) {
        syntax(ts, w);
        return 0;
    }
    ts->pos++;
    return 1;
}

static Node *parse_list(Tokens *ts);

static int at_list_end(Tokens *ts) {
    Token *t = peek(ts);
    if (t->type == T_EOF) return 1;
    if (t->type == T_OP && (!strcmp(t->text, ")") || !strcmp(t->text, ";;"))) return 1;
    if (t->type == T_WORD) {
        static const char *const kw[] = {"then", "elif", "else", "fi", "do", "done", "}", NULL};
        for (int i = 0; kw[i]; i++)
            if (!strcmp(t->text, kw[i])) return 1;
    }
    return 0;
}

static void parse_redirs(Tokens *ts, Node *n) {
    while (peek(ts)->type == T_REDIR) {
        Token *r = peek(ts);
        ts->pos++;
        if (peek(ts)->type != T_WORD) {
            syntax(ts, "redirection target");
            return;
        }
        Redir *rd = xmalloc(sizeof *rd);
        rd->fd = r->fd;
        rd->kind = r->kind;
        rd->word = peek(ts)->text;
        rd->next = NULL;
        ts->pos++;
        Redir **pp = &n->redir;
        while (*pp) pp = &(*pp)->next;
        *pp = rd;
    }
}

static Node *parse_command(Tokens *ts) {
    if (parse_error) return NULL;
    Node *n;
    if (is_word(ts, "if")) {
        ts->pos++;
        n = node(N_IF);
        Node *cur = n;
        cur->a = parse_list(ts);
        if (!expect_word(ts, "then")) return NULL;
        cur->b = parse_list(ts);
        for (;;) {
            skip_nl(ts);
            if (is_word(ts, "elif")) {
                ts->pos++;
                Node *e = node(N_IF);
                e->a = parse_list(ts);
                if (!expect_word(ts, "then")) return NULL;
                e->b = parse_list(ts);
                cur->c = e;
                cur = e;
            } else if (is_word(ts, "else")) {
                ts->pos++;
                cur->c = parse_list(ts);
            } else {
                break;
            }
        }
        if (!expect_word(ts, "fi")) return NULL;
    } else if (is_word(ts, "while") || is_word(ts, "until")) {
        n = node(is_word(ts, "while") ? N_WHILE : N_UNTIL);
        ts->pos++;
        n->a = parse_list(ts);
        if (!expect_word(ts, "do")) return NULL;
        n->b = parse_list(ts);
        if (!expect_word(ts, "done")) return NULL;
    } else if (is_word(ts, "for")) {
        ts->pos++;
        n = node(N_FOR);
        if (peek(ts)->type != T_WORD) {
            syntax(ts, "for variable");
            return NULL;
        }
        n->var = peek(ts)->text;
        ts->pos++;
        skip_nl(ts);
        if (is_word(ts, "in")) {
            ts->pos++;
            while (peek(ts)->type == T_WORD) {
                list_push(&n->words, peek(ts)->text);
                ts->pos++;
            }
        } else {
            list_push(&n->words, "\"$@\"");
        }
        if (is_op(ts, ";")) ts->pos++;
        if (!expect_word(ts, "do")) return NULL;
        n->b = parse_list(ts);
        if (!expect_word(ts, "done")) return NULL;
    } else if (is_op(ts, "(")) {
        ts->pos++;
        n = node(N_SUBSHELL);
        n->a = parse_list(ts);
        skip_nl(ts);
        if (!is_op(ts, ")")) {
            syntax(ts, ")");
            return NULL;
        }
        ts->pos++;
    } else if (is_word(ts, "{")) {
        ts->pos++;
        n = node(N_GROUP);
        n->a = parse_list(ts);
        if (!expect_word(ts, "}")) return NULL;
    } else if (peek(ts)->type == T_WORD && ts->t[ts->pos + 1].type == T_OP && !strcmp(ts->t[ts->pos + 1].text, "(") &&
               ts->t[ts->pos + 2].type == T_OP && !strcmp(ts->t[ts->pos + 2].text, ")")) {
        /* name() compound-command */
        n = node(N_FUNC);
        n->var = peek(ts)->text;
        ts->pos += 3;
        skip_nl(ts);
        n->a = parse_command(ts);
        return n;
    } else {
        n = node(N_CMD);
        while (!parse_error) {
            Token *t = peek(ts);
            if (t->type == T_WORD) {
                list_push(&n->words, t->text);
                ts->pos++;
            } else if (t->type == T_REDIR) {
                parse_redirs(ts, n);
            } else {
                break;
            }
        }
        if (!n->words.n && !n->redir) {
            syntax(ts, "command");
            return NULL;
        }
        return n;
    }
    parse_redirs(ts, n);
    return n;
}

static Node *parse_pipeline(Tokens *ts) {
    int negate = 0;
    if (is_word(ts, "!")) {
        negate = 1;
        ts->pos++;
    }
    Node *n = parse_command(ts);
    while (!parse_error && is_op(ts, "|")) {
        ts->pos++;
        skip_nl(ts);
        Node *p = node(N_PIPE);
        p->a = n;
        p->b = parse_command(ts);
        n = p;
    }
    if (negate) {
        Node *x = node(N_NOT);
        x->a = n;
        n = x;
    }
    return n;
}

static Node *parse_and_or(Tokens *ts) {
    Node *n = parse_pipeline(ts);
    while (!parse_error && (is_op(ts, "&&") || is_op(ts, "||"))) {
        Node *p = node(is_op(ts, "&&") ? N_AND : N_OR);
        ts->pos++;
        skip_nl(ts);
        p->a = n;
        p->b = parse_pipeline(ts);
        n = p;
    }
    return n;
}

static Node *parse_list(Tokens *ts) {
    Node *head = NULL;
    skip_nl(ts);
    while (!parse_error && !at_list_end(ts)) {
        Node *n = parse_and_or(ts);
        if (parse_error) break;
        if (is_op(ts, "&")) {
            Node *b = node(N_BG);
            b->a = n;
            n = b;
            ts->pos++;
        } else if (is_op(ts, ";") || peek(ts)->type == T_NL) {
            ts->pos++;
        } else if (!at_list_end(ts)) {
            syntax(ts, "end of command");
            break;
        }
        if (!head) {
            head = n;
        } else {
            Node *s = node(N_SEQ);
            s->a = head;
            s->b = n;
            head = s;
        }
        skip_nl(ts);
    }
    return head;
}

/* ------------------------------------------------------------------ expansion */

static int run_string(const char *src);
static char *expand_one(const char *w);

static long arith(const char **p);

static long arith_atom(const char **p) {
    while (isspace((unsigned char)**p)) (*p)++;
    if (**p == '(') {
        (*p)++;
        long v = arith(p);
        while (isspace((unsigned char)**p)) (*p)++;
        if (**p == ')') (*p)++;
        return v;
    }
    if (**p == '-') {
        (*p)++;
        return -arith_atom(p);
    }
    if (**p == '+') {
        (*p)++;
        return arith_atom(p);
    }
    if (**p == '!') {
        (*p)++;
        return !arith_atom(p);
    }
    if (**p == '$') (*p)++;
    if (isdigit((unsigned char)**p)) return strtol(*p, (char **)p, 0);
    const char *s = *p;
    while (isalnum((unsigned char)**p) || **p == '_') (*p)++;
    if (*p == s) return 0;
    char *name = xstrndup(s, *p - s);
    const char *v = var_get(name);
    free(name);
    return v ? strtol(v, NULL, 0) : 0;
}

static int arith_prec(const char *op) {
    static const struct { const char *op; int prec; } t[] = {
        {"||", 1}, {"&&", 2}, {"==", 3}, {"!=", 3}, {"<=", 4}, {">=", 4}, {"<", 4}, {">", 4},
        {"+", 5},  {"-", 5},  {"*", 6},  {"/", 6},  {"%", 6},
    };
    for (unsigned i = 0; i < sizeof t / sizeof *t; i++)
        if (!strncmp(op, t[i].op, strlen(t[i].op))) return t[i].prec;
    return 0;
}

static long arith_bin(const char **p, int min) {
    long l = arith_atom(p);
    for (;;) {
        while (isspace((unsigned char)**p)) (*p)++;
        int prec = arith_prec(*p);
        if (!prec || prec < min) return l;
        char op[3] = {(*p)[0], 0, 0};
        if (((*p)[1] == '=' || (*p)[1] == '&' || (*p)[1] == '|') && strchr("=!<>&|", op[0])) op[1] = (*p)[1];
        *p += strlen(op);
        long r = arith_bin(p, prec + 1);
        if (!strcmp(op, "+")) l += r;
        else if (!strcmp(op, "-")) l -= r;
        else if (!strcmp(op, "*")) l *= r;
        else if (!strcmp(op, "/")) l = r ? l / r : 0;
        else if (!strcmp(op, "%")) l = r ? l % r : 0;
        else if (!strcmp(op, "<")) l = l < r;
        else if (!strcmp(op, ">")) l = l > r;
        else if (!strcmp(op, "<=")) l = l <= r;
        else if (!strcmp(op, ">=")) l = l >= r;
        else if (!strcmp(op, "==")) l = l == r;
        else if (!strcmp(op, "!=")) l = l != r;
        else if (!strcmp(op, "&&")) l = l && r;
        else if (!strcmp(op, "||")) l = l || r;
    }
}

static long arith(const char **p) { return arith_bin(p, 1); }

/* Output of a command, trailing newlines removed. */
static char *command_output(const char *cmd) {
    int fds[2];
    if (pipe(fds) < 0) return strdup("");
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        close(fds[0]);
        dup2(fds[1], 1);
        close(fds[1]);
        interactive = 0;
        int st = run_string(cmd);
        fflush(stdout);
        _exit(st);
    }
    close(fds[1]);
    Str out = {0};
    str_puts(&out, "");
    char buf[512];
    ssize_t n;
    while ((n = read(fds[0], buf, sizeof buf)) > 0 || (n < 0 && errno == EINTR))
        if (n > 0) str_putn(&out, buf, n);
    close(fds[0]);
    int st;
    while (waitpid(p, &st, 0) < 0 && errno == EINTR) {}
    last_status = WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
    while (out.len && out.s[out.len - 1] == '\n') out.s[--out.len] = 0;
    return out.s;
}

/* Glob matching for one path component. */
static int match(const char *pat, const char *s) {
    for (; *pat; pat++, s++) {
        if (*pat == '*') {
            while (pat[1] == '*') pat++;
            if (!pat[1]) return 1;
            for (; *s; s++)
                if (match(pat + 1, s)) return 1;
            return 0;
        }
        if (!*s) return 0;
        if (*pat == '?') continue;
        if (*pat == '[') {
            const char *q = pat + 1;
            int neg = *q == '!' || *q == '^', ok = 0;
            if (neg) q++;
            for (; *q && *q != ']'; q++) {
                if (q[1] == '-' && q[2] && q[2] != ']') {
                    if (*s >= q[0] && *s <= q[2]) ok = 1;
                    q += 2;
                } else if (*q == *s) {
                    ok = 1;
                }
            }
            if (!*q || ok == neg) return 0;
            pat = q;
            continue;
        }
        if (*pat == '\\' && pat[1]) pat++;
        if (*pat != *s) return 0;
    }
    return !*s;
}

static int cmp_str(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

static void glob_into(List *out, const char *pat) {
    const char *slash = strrchr(pat, '/');
    char *dir = slash ? xstrndup(pat, slash == pat ? 1 : slash - pat) : strdup(".");
    const char *base = slash ? slash + 1 : pat;
    DIR *d = opendir(dir);
    int found = 0;
    if (d) {
        List hits = {0};
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.' && base[0] != '.') continue;
            if (!match(base, e->d_name)) continue;
            Str s = {0};
            if (slash) {
                str_putn(&s, pat, slash - pat + 1);
            }
            str_puts(&s, e->d_name);
            list_push(&hits, s.s);
        }
        closedir(d);
        if (hits.n) {
            qsort(hits.v, hits.n, sizeof *hits.v, cmp_str);
            for (int i = 0; i < hits.n; i++) list_push(out, hits.v[i]);
            found = 1;
        }
        free(hits.v);
    }
    free(dir);
    if (!found) {
        /* No match: the pattern stays, minus its backslashes. */
        Str s = {0};
        str_puts(&s, "");
        for (const char *p = pat; *p; p++) {
            if (*p == '\\' && p[1]) p++;
            str_putc(&s, *p);
        }
        list_push(out, s.s);
    }
}

typedef struct {
    Str cur;    /* current field */
    Str pat;    /* same field with glob metacharacters from quotes escaped */
    int active; /* a field exists (even if empty) */
    int glob;   /* unquoted glob characters seen */
    List *out;
    int split;  /* split unquoted expansions and glob (off for assignments, redirections) */
} Exp;

static void exp_char(Exp *e, char c, int quoted) {
    str_putc(&e->cur, c);
    if (quoted && strchr("*?[]\\", c)) str_putc(&e->pat, '\\');
    str_putc(&e->pat, c);
    if (!quoted && strchr("*?[", c)) e->glob = 1;
    e->active = 1;
}

static void exp_end_field(Exp *e) {
    if (e->active) {
        if (e->glob && e->split) glob_into(e->out, e->pat.s);
        else list_push(e->out, strdup(e->cur.s ? e->cur.s : ""));
    }
    free(e->cur.s);
    free(e->pat.s);
    e->cur = (Str){0};
    e->pat = (Str){0};
    e->active = e->glob = 0;
}

static void exp_value(Exp *e, const char *v, int quoted) {
    if (!v) return;
    if (quoted || !e->split) {
        for (; *v; v++) exp_char(e, *v, 1);
        if (quoted) e->active = 1;
        return;
    }
    for (; *v; v++) {
        if (isspace((unsigned char)*v)) {
            exp_end_field(e);
            while (isspace((unsigned char)v[1])) v++;
        } else {
            exp_char(e, *v, 1);
        }
    }
}

/* Expand one $-form starting at p (just past '$'); returns the position after it. */
static const char *expand_dollar(Exp *e, const char *p, int quoted) {
    if (p[0] == '(' && p[1] == '(') {
        const char *end = skip_parens(p);
        if (!end) return p;
        char *raw = xstrndup(p + 2, end - p - 4 > 0 ? end - p - 4 : 0);
        char *expr = expand_one(raw); /* $vars and $(commands) inside are expanded first */
        const char *q = expr;
        char buf[32];
        snprintf(buf, sizeof buf, "%ld", arith(&q));
        free(raw);
        free(expr);
        exp_value(e, buf, quoted);
        return end;
    }
    if (p[0] == '(') {
        const char *end = skip_parens(p);
        if (!end) return p;
        char *cmd = xstrndup(p + 1, end - p - 2);
        char *out = command_output(cmd);
        exp_value(e, out, quoted);
        free(cmd);
        free(out);
        return end;
    }
    char buf[32];
    if (*p == '?') {
        snprintf(buf, sizeof buf, "%d", last_status);
        exp_value(e, buf, quoted);
        return p + 1;
    }
    if (*p == '$') {
        snprintf(buf, sizeof buf, "%d", getpid());
        exp_value(e, buf, quoted);
        return p + 1;
    }
    if (*p == '#') {
        snprintf(buf, sizeof buf, "%d", script_argc > 0 ? script_argc - 1 : 0);
        exp_value(e, buf, quoted);
        return p + 1;
    }
    if (isdigit((unsigned char)*p)) {
        int i = *p - '0';
        if (i < script_argc) exp_value(e, script_argv[i], quoted);
        return p + 1;
    }
    if (*p == '@' || *p == '*') {
        for (int i = 1; i < script_argc; i++) {
            if (i > 1) {
                if (quoted && *p == '@') {
                    e->active = 1;
                    exp_end_field(e);
                } else {
                    exp_value(e, " ", quoted);
                }
            }
            exp_value(e, script_argv[i], quoted);
        }
        return p + 1;
    }
    if (*p == '{') {
        const char *end = strchr(p, '}');
        if (!end) return p;
        const char *colon = memchr(p, ':', end - p);
        size_t nl = (colon ? colon : end) - (p + 1);
        char *name = xstrndup(p + 1, nl);
        const char *v;
        if (!strcmp(name, "#")) {
            snprintf(buf, sizeof buf, "%d", script_argc > 0 ? script_argc - 1 : 0);
            v = buf;
        } else if (!strcmp(name, "?")) {
            snprintf(buf, sizeof buf, "%d", last_status);
            v = buf;
        } else {
            v = var_get(name);
        }
        if (colon && colon[1] == '-' && (!v || !*v)) {
            char *def = xstrndup(colon + 2, end - colon - 2);
            exp_value(e, def, quoted);
            free(def);
        } else if (colon && colon[1] == '=' && (!v || !*v)) {
            char *def = xstrndup(colon + 2, end - colon - 2);
            var_set(name, def, 0);
            exp_value(e, def, quoted);
            free(def);
        } else {
            exp_value(e, v, quoted);
        }
        free(name);
        return end + 1;
    }
    if (isalpha((unsigned char)*p) || *p == '_') {
        const char *s = p;
        while (isalnum((unsigned char)*p) || *p == '_') p++;
        char *name = xstrndup(s, p - s);
        exp_value(e, var_get(name), quoted);
        free(name);
        return p;
    }
    exp_char(e, '$', 1);
    return p;
}

static void expand(const char *w, List *out, int split) {
    Exp e = {{0}, {0}, 0, 0, out, split};
    const char *p = w;
    /* Tilde at the start. */
    if (p[0] == '~' && (p[1] == '/' || !p[1])) {
        const char *home = var_get("HOME");
        exp_value(&e, home ? home : "/", 1);
        p++;
    }
    while (*p) {
        if (*p == '\\' && p[1]) {
            exp_char(&e, p[1], 1);
            p += 2;
        } else if (*p == '\'') {
            const char *end = strchr(p + 1, '\'');
            if (!end) end = p + strlen(p);
            e.active = 1;
            for (const char *q = p + 1; q < end; q++) exp_char(&e, *q, 1);
            p = *end ? end + 1 : end;
        } else if (*p == '"') {
            p++;
            e.active = 1;
            while (*p && *p != '"') {
                if (*p == '\\' && p[1] && strchr("$`\"\\\n", p[1])) {
                    exp_char(&e, p[1], 1);
                    p += 2;
                } else if (*p == '$') {
                    p = expand_dollar(&e, p + 1, 1);
                } else {
                    exp_char(&e, *p++, 1);
                }
            }
            if (*p) p++;
        } else if (*p == '$') {
            p = expand_dollar(&e, p + 1, 0);
        } else {
            exp_char(&e, *p++, 0);
        }
    }
    exp_end_field(&e);
}

static char *expand_one(const char *w) {
    List l = {0};
    expand(w, &l, 0);
    Str s = {0};
    str_puts(&s, "");
    for (int i = 0; i < l.n; i++) {
        if (i) str_putc(&s, ' ');
        str_puts(&s, l.v[i]);
        free(l.v[i]);
    }
    free(l.v);
    return s.s;
}

/* ------------------------------------------------------------------ redirections */

typedef struct {
    int fd, saved;
} Saved;

static int apply_redirs(Redir *r, Saved *save, int *nsave) {
    for (; r; r = r->next) {
        char *target = expand_one(r->word);
        int fd;
        if (r->kind == R_DUPOUT || r->kind == R_DUPIN) {
            if (!strcmp(target, "-")) {
                fd = -1;
            } else {
                fd = atoi(target);
                if (fcntl(fd, F_GETFD) < 0) {
                    fprintf(stderr, "sh: %s: bad file descriptor\n", target);
                    free(target);
                    return -1;
                }
            }
        } else {
            int fl = r->kind == R_IN ? O_RDONLY
                     : r->kind == R_RDWR ? O_RDWR | O_CREAT
                     : r->kind == R_APPEND ? O_WRONLY | O_CREAT | O_APPEND
                                           : O_WRONLY | O_CREAT | O_TRUNC;
            fd = open(target, fl, 0644);
            if (fd < 0) {
                fprintf(stderr, "sh: %s: %s\n", target, strerror(errno));
                free(target);
                return -1;
            }
        }
        free(target);
        if (save) {
            save[*nsave].fd = r->fd;
            save[*nsave].saved = fcntl(r->fd, F_DUPFD_CLOEXEC, 10);
            (*nsave)++;
        }
        if (fd < 0) {
            close(r->fd);
        } else if (fd != r->fd) {
            dup2(fd, r->fd);
            if (r->kind != R_DUPOUT && r->kind != R_DUPIN) close(fd);
        }
    }
    return 0;
}

static void restore_redirs(Saved *save, int n) {
    while (n-- > 0) {
        if (save[n].saved >= 0) {
            dup2(save[n].saved, save[n].fd);
            close(save[n].saved);
        } else {
            close(save[n].fd);
        }
    }
}

/* ------------------------------------------------------------------ builtins */

static int builtin_test(int argc, char **argv);

static int test_unary(const char *op, const char *a) {
    struct stat st;
    switch (op[1]) {
    case 'z': return !*a;
    case 'n': return *a != 0;
    case 'e': return stat(a, &st) == 0;
    case 'f': return stat(a, &st) == 0 && S_ISREG(st.st_mode);
    case 'd': return stat(a, &st) == 0 && S_ISDIR(st.st_mode);
    case 'p': return stat(a, &st) == 0 && S_ISFIFO(st.st_mode);
    case 'c': return stat(a, &st) == 0 && S_ISCHR(st.st_mode);
    case 's': return stat(a, &st) == 0 && st.st_size > 0;
    case 'r': return access(a, R_OK) == 0;
    case 'w': return access(a, W_OK) == 0;
    case 'x': return access(a, X_OK) == 0;
    case 't': return isatty(atoi(a));
    }
    return -1;
}

static int test_expr(int argc, char **argv) {
    if (argc == 0) return 0;
    if (!strcmp(argv[0], "!")) return !test_expr(argc - 1, argv + 1);
    if (argc == 1) return argv[0][0] != 0;
    if (argc == 2 && argv[0][0] == '-') {
        int r = test_unary(argv[0], argv[1]);
        return r < 0 ? 0 : r;
    }
    if (argc >= 3) {
        /* -a / -o split (lowest precedence) */
        for (int i = 1; i < argc - 1; i++) {
            if (!strcmp(argv[i], "-o")) return test_expr(i, argv) || test_expr(argc - i - 1, argv + i + 1);
        }
        for (int i = 1; i < argc - 1; i++) {
            if (!strcmp(argv[i], "-a")) return test_expr(i, argv) && test_expr(argc - i - 1, argv + i + 1);
        }
        const char *a = argv[0], *op = argv[1], *b = argv[2];
        if (!strcmp(op, "=") || !strcmp(op, "==")) return !strcmp(a, b);
        if (!strcmp(op, "!=")) return strcmp(a, b) != 0;
        long x = strtol(a, NULL, 10), y = strtol(b, NULL, 10);
        if (!strcmp(op, "-eq")) return x == y;
        if (!strcmp(op, "-ne")) return x != y;
        if (!strcmp(op, "-lt")) return x < y;
        if (!strcmp(op, "-le")) return x <= y;
        if (!strcmp(op, "-gt")) return x > y;
        if (!strcmp(op, "-ge")) return x >= y;
    }
    return 0;
}

static int builtin_test(int argc, char **argv) {
    if (!strcmp(argv[0], "[")) {
        if (strcmp(argv[argc - 1], "]")) {
            fputs("sh: [: missing ]\n", stderr);
            return 2;
        }
        argc--;
    }
    return !test_expr(argc - 1, argv + 1);
}

static int builtin_echo(int argc, char **argv) {
    int i = 1, nl = 1, esc = 0;
    for (; i < argc && argv[i][0] == '-' && argv[i][1]; i++) {
        const char *f = argv[i] + 1;
        if (strspn(f, "neE") != strlen(f)) break;
        for (; *f; f++) {
            if (*f == 'n') nl = 0;
            else if (*f == 'e') esc = 1;
            else esc = 0;
        }
    }
    for (int first = 1; i < argc; i++, first = 0) {
        if (!first) putchar(' ');
        for (const char *s = argv[i]; *s; s++) {
            if (esc && *s == '\\' && s[1]) {
                s++;
                switch (*s) {
                case 'n': putchar('\n'); break;
                case 't': putchar('\t'); break;
                case '\\': putchar('\\'); break;
                case 'c': fflush(stdout); return 0;
                default: putchar('\\'); putchar(*s);
                }
            } else {
                putchar(*s);
            }
        }
    }
    if (nl) putchar('\n');
    fflush(stdout);
    return ferror(stdout) ? 1 : 0;
}

static int builtin_read(int argc, char **argv) {
    Str line = {0};
    str_puts(&line, "");
    char c;
    ssize_t n;
    int got = 0;
    while ((n = read(0, &c, 1)) > 0) {
        got = 1;
        if (c == '\n') break;
        str_putc(&line, c);
    }
    if (!got) {
        free(line.s);
        return 1;
    }
    char *p = line.s;
    for (int i = 1; i < argc; i++) {
        while (isspace((unsigned char)*p)) p++;
        char *start = p;
        if (i == argc - 1) {
            size_t l = strlen(p);
            while (l && isspace((unsigned char)p[l - 1])) p[--l] = 0;
            var_set(argv[i], start, 0);
            break;
        }
        while (*p && !isspace((unsigned char)*p)) p++;
        char save = *p;
        *p = 0;
        var_set(argv[i], start, 0);
        *p = save;
    }
    if (argc == 1) var_set("REPLY", line.s, 0);
    free(line.s);
    return 0;
}

static int source_file(const char *path);

static int run_builtin(int argc, char **argv, int *handled) {
    const char *c = argv[0];
    *handled = 1;
    if (!strcmp(c, "cd")) {
        const char *dir = argc > 1 ? argv[1] : var_get("HOME");
        if (!dir) dir = "/";
        if (chdir(dir) < 0) {
            fprintf(stderr, "sh: cd: %s: %s\n", dir, strerror(errno));
            return 1;
        }
        char buf[4096];
        if (getcwd(buf, sizeof buf)) var_set("PWD", buf, 1);
        return 0;
    }
    if (!strcmp(c, "pwd")) {
        char buf[4096];
        if (!getcwd(buf, sizeof buf)) {
            perror("sh: pwd");
            return 1;
        }
        puts(buf);
        fflush(stdout);
        return 0;
    }
    if (!strcmp(c, "exit")) {
        exiting = 1;
        exit_code = argc > 1 ? atoi(argv[1]) : last_status;
        return exit_code;
    }
    if (!strcmp(c, "export") || !strcmp(c, "unset")) {
        int unset = c[0] == 'u';
        for (int i = 1; i < argc; i++) {
            char *eq = strchr(argv[i], '=');
            if (unset) {
                var_unset(argv[i]);
            } else if (eq) {
                *eq = 0;
                var_set(argv[i], eq + 1, 1);
                *eq = '=';
            } else {
                var_set(argv[i], NULL, 1);
            }
        }
        if (!unset && argc == 1)
            for (Var *v = vars; v; v = v->next)
                if (v->exported) printf("export %s=\"%s\"\n", v->name, v->value ? v->value : "");
        fflush(stdout);
        return 0;
    }
    if (!strcmp(c, "set")) {
        if (argc > 1) {
            /* set -- args: replace the positional parameters */
            int i = 1;
            if (!strcmp(argv[1], "--")) i = 2;
            static List pos;
            pos.n = 0;
            list_push(&pos, script_argc > 0 ? script_argv[0] : "sh");
            for (; i < argc; i++) list_push(&pos, strdup(argv[i]));
            script_argc = pos.n;
            script_argv = pos.v;
            return 0;
        }
        for (Var *v = vars; v; v = v->next) printf("%s=%s\n", v->name, v->value ? v->value : "");
        fflush(stdout);
        return 0;
    }
    if (!strcmp(c, "shift")) {
        int n = argc > 1 ? atoi(argv[1]) : 1;
        if (n >= script_argc) n = script_argc - 1;
        if (n > 0) {
            for (int i = 1; i + n < script_argc; i++) script_argv[i] = script_argv[i + n];
            script_argc -= n;
            script_argv[script_argc] = NULL;
        }
        return 0;
    }
    if (!strcmp(c, "return")) {
        returning = 1;
        return last_status = argc > 1 ? atoi(argv[1]) : last_status;
    }
    if (!strcmp(c, "echo")) return builtin_echo(argc, argv);
    if (!strcmp(c, "true") || !strcmp(c, ":")) return 0;
    if (!strcmp(c, "false")) return 1;
    if (!strcmp(c, "test") || !strcmp(c, "[")) return builtin_test(argc, argv);
    if (!strcmp(c, "read")) return builtin_read(argc, argv);
    if (!strcmp(c, "source") || !strcmp(c, ".")) {
        if (argc < 2) return 2;
        return source_file(argv[1]);
    }
    if (!strcmp(c, "wait")) {
        int st = 0;
        if (argc > 1) {
            if (waitpid(atoi(argv[1]), &st, 0) < 0) return 127;
            return WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
        }
        while (wait(&st) > 0 || errno == EINTR) {}
        return 0;
    }
    if (!strcmp(c, "help")) {
        puts("Citadel sh built-ins: cd pwd echo export unset set shift read test [ source . exec wait exit true false help\n"
             "Syntax: | && || ; & ( ) { } < > >> 2>&1 $VAR ${VAR:-x} $((expr)) $(cmd) * ? [..]\n"
             "        if/elif/else/fi, while/until/do/done, for NAME in WORDS; do ...; done\n"
             "Programs live in /bin: ls cat echo grep wc sort head tail cp mv rm mkdir rmdir touch ln\n"
             "        ps kill sleep date uname env seq yes stat lua httpd bench ...");
        fflush(stdout);
        return 0;
    }
    *handled = 0;
    return 0;
}

/* ------------------------------------------------------------------ functions */

typedef struct Func {
    char *name;
    Node *body;
    struct Func *next;
} Func;
static Func *funcs;

static Func *func_find(const char *name) {
    for (Func *f = funcs; f; f = f->next)
        if (!strcmp(f->name, name)) return f;
    return NULL;
}

static int exec_node(Node *n, int fork_ok);


static int call_function(Func *f, int argc, char **argv) {
    int saved_argc = script_argc;
    char **saved_argv = script_argv;
    char **args = xmalloc((argc + 1) * sizeof *args);
    args[0] = saved_argc > 0 ? saved_argv[0] : "sh";
    for (int i = 1; i < argc; i++) args[i] = strdup(argv[i]);
    args[argc] = NULL;
    script_argc = argc;
    script_argv = args;
    int st = exec_node(f->body, 1);
    if (returning) {
        st = last_status;
        returning = 0;
    }
    for (int i = 1; i < argc; i++) free(args[i]);
    free(args);
    script_argc = saved_argc;
    script_argv = saved_argv;
    return st;
}

/* ------------------------------------------------------------------ execution */

static int status_of(int st) {
    if (WIFEXITED(st)) return WEXITSTATUS(st);
    if (WIFSIGNALED(st)) {
        int sig = WTERMSIG(st);
        if (sig != SIGINT && sig != SIGPIPE) fprintf(stderr, "%s\n", strsignal(sig));
        else if (sig == SIGINT && interactive) fputc('\n', stderr);
        return 128 + sig;
    }
    return 1;
}

static void child_signals(void) {
    signal(SIGINT, SIG_DFL);
    signal(SIGQUIT, SIG_DFL);
    signal(SIGTSTP, SIG_DFL);
    signal(SIGTTOU, SIG_DFL);
    signal(SIGTTIN, SIG_DFL);
}

/* Foreground job: put it in its own process group and give it the terminal. */
static void fg_setup(pid_t pid, pid_t pgid) {
    if (!interactive) return;
    setpgid(pid, pgid);
    tcsetpgrp(0, pgid);
}

static int fg_wait(pid_t pid) {
    int st;
    while (waitpid(pid, &st, 0) < 0)
        if (errno != EINTR) return 127;
    if (interactive) tcsetpgrp(0, shell_pgid);
    return status_of(st);
}

static void exec_external(char **argv, List *assigns) {
    char **env = build_env(assigns);
    const char *path = argv[0];
    if (strchr(path, '/')) {
        execve(path, argv, env);
    } else {
        const char *pathvar = var_get("PATH");
        if (!pathvar) pathvar = "/bin";
        char buf[4096];
        const char *p = pathvar;
        int saw_eacces = 0;
        while (*p) {
            size_t l = strcspn(p, ":");
            snprintf(buf, sizeof buf, "%.*s/%s", (int)l, p, path);
            execve(buf, argv, env);
            if (errno == EACCES) saw_eacces = 1;
            p += l;
            if (*p == ':') p++;
        }
        if (saw_eacces) errno = EACCES;
    }
    if (errno == ENOENT) {
        fprintf(stderr, "sh: %s: command not found\n", argv[0]);
        _exit(127);
    }
    fprintf(stderr, "sh: %s: %s\n", argv[0], strerror(errno));
    _exit(126);
}

static int exec_cmd(Node *n, int fork_ok) {
    List words = {0}, assigns = {0};
    int i = 0;
    /* leading NAME=value words */
    for (; i < n->words.n && is_assignment(n->words.v[i]); i++) {
        char *eq = strchr(n->words.v[i], '=');
        Str s = {0};
        str_putn(&s, n->words.v[i], eq - n->words.v[i] + 1);
        char *val = expand_one(eq + 1);
        str_puts(&s, val);
        free(val);
        list_push(&assigns, s.s);
    }
    for (; i < n->words.n; i++) expand(n->words.v[i], &words, 1);
    int status = 0;
    if (words.n == 0) {
        for (int k = 0; k < assigns.n; k++) {
            char *eq = strchr(assigns.v[k], '=');
            *eq = 0;
            var_set(assigns.v[k], eq + 1, 0);
        }
        Saved sv[16];
        int ns = 0;
        if (n->redir) {
            status = apply_redirs(n->redir, sv, &ns) < 0 ? 1 : 0;
            restore_redirs(sv, ns);
        }
        goto out;
    }
    if (!strcmp(words.v[0], "exec")) {
        if (apply_redirs(n->redir, NULL, NULL) < 0) {
            status = 1;
            goto out;
        }
        if (words.n > 1) exec_external(words.v + 1, &assigns);
        goto out;
    }
    {
        Saved sv[16];
        int ns = 0;
        int handled;
        if (apply_redirs(n->redir, sv, &ns) < 0) {
            restore_redirs(sv, ns);
            status = 1;
            goto out;
        }
        Func *fn = func_find(words.v[0]);
        if (fn) {
            status = call_function(fn, words.n, words.v);
            handled = 1;
        } else {
            status = run_builtin(words.n, words.v, &handled);
        }
        fflush(stdout);
        restore_redirs(sv, ns);
        if (handled) goto out;
    }
    if (!fork_ok) {
        /* already in a child (pipeline member or subshell): exec directly */
        if (apply_redirs(n->redir, NULL, NULL) < 0) _exit(1);
        exec_external(words.v, &assigns);
    }
    fflush(stdout);
    pid_t pid = fork();
    if (pid < 0) {
        perror("sh: fork");
        status = 1;
        goto out;
    }
    if (pid == 0) {
        if (interactive) {
            setpgid(0, 0);
            tcsetpgrp(0, getpid());
        }
        child_signals();
        if (apply_redirs(n->redir, NULL, NULL) < 0) _exit(1);
        exec_external(words.v, &assigns);
    }
    fg_setup(pid, pid);
    status = fg_wait(pid);
out:
    for (int k = 0; k < words.n; k++) free(words.v[k]);
    for (int k = 0; k < assigns.n; k++) free(assigns.v[k]);
    free(words.v);
    free(assigns.v);
    return status;
}

static int exec_pipeline(Node *n) {
    /* Flatten the left-leaning PIPE chain. */
    Node *parts[64];
    int np = 0;
    Node *stack[64];
    int sp = 0;
    for (Node *x = n; x->type == N_PIPE && sp < 63; x = x->a) stack[sp++] = x;
    parts[np++] = stack[sp - 1]->a;
    while (sp > 0) parts[np++] = stack[--sp]->b;
    pid_t pids[64], pgid = 0;
    int in = -1;
    fflush(stdout);
    for (int i = 0; i < np; i++) {
        int fds[2] = {-1, -1};
        if (i < np - 1 && pipe(fds) < 0) {
            perror("sh: pipe");
            break;
        }
        pid_t p = fork();
        if (p == 0) {
            if (interactive) {
                setpgid(0, pgid ? pgid : getpid());
                if (!pgid) tcsetpgrp(0, getpid());
            }
            child_signals();
            if (in >= 0) {
                dup2(in, 0);
                close(in);
            }
            if (fds[1] >= 0) {
                dup2(fds[1], 1);
                close(fds[1]);
                close(fds[0]);
            }
            interactive = 0;
            int st = exec_node(parts[i], 0);
            fflush(stdout);
            _exit(st);
        }
        if (p < 0) perror("sh: fork");
        if (!pgid) pgid = p;
        if (interactive) setpgid(p, pgid);
        pids[i] = p;
        if (in >= 0) close(in);
        if (fds[1] >= 0) close(fds[1]);
        in = fds[0];
    }
    if (in >= 0) close(in);
    if (interactive) tcsetpgrp(0, pgid);
    int status = 0;
    for (int i = 0; i < np; i++) {
        int st;
        while (waitpid(pids[i], &st, 0) < 0 && errno == EINTR) {}
        if (i == np - 1) status = status_of(st);
    }
    if (interactive) tcsetpgrp(0, shell_pgid);
    return status;
}

static int in_subshell(Node *n, int fork_ok, int status_only) {
    (void)status_only;
    if (!fork_ok) {
        int st = exec_node(n, 1);
        fflush(stdout);
        _exit(st);
    }
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        if (interactive) {
            setpgid(0, 0);
            tcsetpgrp(0, getpid());
        }
        child_signals();
        interactive = 0;
        int st = exec_node(n, 1);
        fflush(stdout);
        _exit(exiting ? exit_code : st);
    }
    fg_setup(p, p);
    return fg_wait(p);
}

static int exec_node(Node *n, int fork_ok) {
    if (!n || exiting || returning) return last_status;
    int st = 0;
    Saved sv[16];
    int ns = 0;
    if (n->type != N_CMD && n->redir) {
        if (apply_redirs(n->redir, sv, &ns) < 0) {
            restore_redirs(sv, ns);
            return last_status = 1;
        }
    }
    switch (n->type) {
    case N_CMD: st = exec_cmd(n, fork_ok); break;
    case N_PIPE: st = exec_pipeline(n); break;
    case N_AND:
        st = exec_node(n->a, 1);
        if (st == 0 && !exiting) st = exec_node(n->b, fork_ok);
        break;
    case N_OR:
        st = exec_node(n->a, 1);
        if (st != 0 && !exiting) st = exec_node(n->b, fork_ok);
        break;
    case N_SEQ:
        exec_node(n->a, 1);
        st = exec_node(n->b, fork_ok);
        break;
    case N_NOT: st = !exec_node(n->a, fork_ok); break;
    case N_BG: {
        fflush(stdout);
        pid_t p = fork();
        if (p == 0) {
            if (interactive) setpgid(0, 0);
            child_signals();
            if (interactive) {
                /* background jobs do not read the terminal */
                int fd = open("/dev/null", O_RDONLY);
                if (fd >= 0) {
                    dup2(fd, 0);
                    close(fd);
                }
            }
            interactive = 0;
            int r = exec_node(n->a, 0);
            fflush(stdout);
            _exit(r);
        }
        if (interactive) {
            setpgid(p, p);
            fprintf(stderr, "[%d]\n", p);
        }
        char buf[16];
        snprintf(buf, sizeof buf, "%d", p);
        var_set("!", buf, 0);
        st = 0;
        break;
    }
    case N_IF:
        if (exec_node(n->a, 1) == 0) st = exec_node(n->b, fork_ok);
        else if (n->c) st = exec_node(n->c, fork_ok);
        else st = 0;
        break;
    case N_WHILE:
    case N_UNTIL:
        st = 0;
        while (!exiting && !returning) {
            int c = exec_node(n->a, 1);
            if ((c == 0) != (n->type == N_WHILE)) break;
            st = exec_node(n->b, 1);
        }
        break;
    case N_FOR: {
        List items = {0};
        for (int i = 0; i < n->words.n; i++) expand(n->words.v[i], &items, 1);
        st = 0;
        for (int i = 0; i < items.n && !exiting && !returning; i++) {
            var_set(n->var, items.v[i], 0);
            st = exec_node(n->b, 1);
        }
        for (int i = 0; i < items.n; i++) free(items.v[i]);
        free(items.v);
        break;
    }
    case N_SUBSHELL: st = in_subshell(n->a, fork_ok, 0); break;
    case N_GROUP: st = exec_node(n->a, fork_ok); break;
    case N_FUNC: {
        Func *f = func_find(n->var);
        if (!f) {
            f = xmalloc(sizeof *f);
            f->name = n->var;
            f->next = funcs;
            funcs = f;
        }
        f->body = n->a;
        st = 0;
        break;
    }
    }
    if (ns) restore_redirs(sv, ns);
    last_status = st;
    return st;
}

/* Parse and run a complete program text. Returns the last status. */
static int run_string(const char *src) {
    Tokens ts = {0};
    lex(src, &ts);
    if (ts.incomplete) {
        fputs("sh: unterminated quote\n", stderr);
        return last_status = 2;
    }
    parse_error = parse_need_more = 0;
    Node *n = parse_list(&ts);
    if (!parse_error && peek(&ts)->type != T_EOF) syntax(&ts, "unexpected token");
    if (parse_error) {
        if (parse_need_more) fputs("sh: unexpected end of input\n", stderr);
        return last_status = 2;
    }
    return exec_node(n, 1);
}

static char *read_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return NULL;
    Str s = {0};
    str_puts(&s, "");
    char buf[4096];
    ssize_t n;
    while ((n = read(fd, buf, sizeof buf)) > 0) str_putn(&s, buf, n);
    close(fd);
    return s.s;
}

static int source_file(const char *path) {
    char *src = read_file(path);
    if (!src) {
        fprintf(stderr, "sh: %s: %s\n", path, strerror(errno));
        return 1;
    }
    int st = run_string(src);
    free(src);
    return st;
}

/* Interactive loop: accumulate lines until the input parses. */
static void repl(void) {
    Str buf = {0};
    char line[4096];
    for (;;) {
        if (exiting) return;
        if (interactive) {
            char cwd[256];
            if (!getcwd(cwd, sizeof cwd)) strcpy(cwd, "?");
            const char *ps = buf.len ? "> " : NULL;
            if (ps) fputs(ps, stderr);
            else fprintf(stderr, "citadel:%s# ", cwd);
        }
        if (!fgets(line, sizeof line, stdin)) {
            if (interactive) fputc('\n', stderr);
            return;
        }
        str_puts(&buf, line);
        Tokens ts = {0};
        lex(buf.s, &ts);
        if (ts.incomplete) continue;
        parse_error = parse_need_more = 0;
        Node *n = parse_list(&ts);
        if (!parse_error && peek(&ts)->type != T_EOF) syntax(&ts, "unexpected token");
        if (parse_error && parse_need_more) continue;
        buf.len = 0;
        buf.s[0] = 0;
        if (parse_error) {
            last_status = 2;
            continue;
        }
        exec_node(n, 1);
        /* reap finished background jobs */
        int st;
        pid_t p;
        while ((p = waitpid(-1, &st, WNOHANG)) > 0)
            if (interactive) fprintf(stderr, "[%d] done (%d)\n", p, status_of(st));
    }
}

int main(int argc, char **argv) {
    for (char **e = environ; e && *e; e++) {
        char *eq = strchr(*e, '=');
        if (!eq) continue;
        *eq = 0;
        var_set(*e, eq + 1, 1);
        *eq = '=';
    }
    if (!var_get("PATH")) var_set("PATH", "/bin", 1);
    script_argc = 1;
    script_argv = argv;
    int force_i = 0;
    int i = 1;
    if (i < argc && !strcmp(argv[i], "-i")) {
        force_i = 1;
        i++;
    }
    if (i < argc && !strcmp(argv[i], "-c")) {
        if (i + 1 >= argc) {
            fputs("sh: -c needs an argument\n", stderr);
            return 2;
        }
        script_argc = argc - (i + 2) + 1;
        script_argv = argv + i + 1;
        if (script_argc < 1) script_argc = 1;
        int st = run_string(argv[i + 1]);
        fflush(stdout);
        return exiting ? exit_code : st;
    }
    if (i < argc) {
        script_argc = argc - i;
        script_argv = argv + i;
        int st = source_file(argv[i]);
        fflush(stdout);
        return exiting ? exit_code : st;
    }
    interactive = force_i || isatty(0);
    if (interactive) {
        signal(SIGINT, SIG_IGN);
        signal(SIGQUIT, SIG_IGN);
        signal(SIGTSTP, SIG_IGN);
        signal(SIGTTOU, SIG_IGN);
        signal(SIGTTIN, SIG_IGN);
        shell_pgid = getpid();
        setpgid(0, shell_pgid);
        tcsetpgrp(0, shell_pgid);
    }
    repl();
    fflush(stdout);
    return exiting ? exit_code : last_status;
}
