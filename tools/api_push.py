# -*- coding: utf-8 -*-
"""经 GitHub REST API（api.github.com 通路）复刻一次本地提交并更新远端 ref。

背景：github.com:443（git push 用的主机）不可达，但 api.github.com 正常。
本脚本用 Git Data API 逐层重建对象：blob -> tree -> commit -> ref。
因为 tree/parent/author/message 全部与本地一致，生成的 commit SHA 与本地完全相同，
故不会造成远端与本地分叉。

用法：
    python tools/api_push.py <本地commit> [tag名]
    # 例：python tools/api_push.py HEAD v0.8.6
    # 只负责提交与标签；Release 与附件另走常规 API（见 baize-release 技能）

注意：api.github.com 建 Release 时若 tag 不存在，会自动把 tag 指向默认分支，
      所以必须先跑本脚本让远端 ref 指向正确 commit，再建 Release。
"""
import base64
import json
import os
import subprocess
import sys
import urllib.request
import urllib.error

REPO = "yubiao-sudo/baize"
API = "https://api.github.com"
REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))  # baize/
LOCAL_COMMIT = sys.argv[1] if len(sys.argv) > 1 else "HEAD"
TAG = sys.argv[2] if len(sys.argv) > 2 else None  # 不传则不建/不动任何 tag


def git(*args, raw=False):
    p = subprocess.run(["git", "-C", REPO_DIR] + list(args),
                       capture_output=True)
    if p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed: {p.stderr.decode('utf-8', 'replace')}")
    return p.stdout if raw else p.stdout.decode("utf-8", "replace").strip()


def token():
    p = subprocess.run(["git", "-C", REPO_DIR, "-c", "credential.helper=wincred",
                        "credential", "fill"],
                       input=b"protocol=https\nhost=github.com\n\n",
                       capture_output=True)
    for line in p.stdout.decode("utf-8", "replace").splitlines():
        if line.startswith("password="):
            return line[len("password="):]
    raise RuntimeError("未取到 GitHub 令牌（wincred）")


TOKEN = token()


def api(method, path, payload=None, raw_body=None, ctype="application/json"):
    url = path if path.startswith("http") else API + path
    data = None
    if raw_body is not None:
        data = raw_body
    elif payload is not None:
        data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Authorization", "Bearer " + TOKEN)
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("Content-Type", ctype)
    req.add_header("User-Agent", "baize-api-push")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            body = r.read()
            return r.status, (json.loads(body) if body else {})
    except urllib.error.HTTPError as e:
        body = e.read()
        try:
            j = json.loads(body)
        except Exception:
            j = {"raw": body.decode("utf-8", "replace")}
        return e.code, j


def main():
    # 1) 本地提交元数据
    hdr = git("cat-file", "commit", LOCAL_COMMIT, raw=True).decode("utf-8", "replace")
    head, _, message = hdr.partition("\n\n")
    tree_sha, parent_sha = None, None
    author_line = committer_line = None
    for line in head.splitlines():
        if line.startswith("tree "):
            tree_sha = line[5:].strip()
        elif line.startswith("parent "):
            parent_sha = line[7:].strip()
        elif line.startswith("author "):
            author_line = line[7:].strip()
        elif line.startswith("committer "):
            committer_line = line[10:].strip()
    print(f"[local] commit={git('rev-parse', LOCAL_COMMIT)} tree={tree_sha} parent={parent_sha}")

    # 2) 远端父提交是否存在（确认 api.github.com 能读到仓库）
    st, remote_parent = api("GET", f"/repos/{REPO}/git/commits/{parent_sha}")
    if st != 200:
        print(f"[err] 远端找不到父提交 {parent_sha} ({st}): {remote_parent}")
        return 1
    print(f"[remote] parent tree={remote_parent['tree']['sha']}")

    # 3) 变更文件清单（相对父提交）
    names = [n for n in git("diff", "--name-only", parent_sha, LOCAL_COMMIT).splitlines() if n]
    print(f"[diff] {len(names)} 个文件")

    entries = []
    for name in names:
        ls = git("ls-tree", LOCAL_COMMIT, "--", name)
        if not ls:
            continue
        meta, _, path = ls.partition("\t")
        mode, _typ, blob_sha = meta.split()
        content = git("cat-file", "blob", blob_sha, raw=True)
        st, resp = api("POST", f"/repos/{REPO}/git/blobs", {
            "content": base64.b64encode(content).decode("ascii"),
            "encoding": "base64",
        })
        if st not in (200, 201):
            print(f"[err] 创建 blob 失败 {name}: {st} {resp}")
            return 1
        if resp["sha"] != blob_sha:
            print(f"[warn] blob sha 不一致 {name}: remote={resp['sha']} local={blob_sha}")
        entries.append({"path": path, "mode": mode, "type": "blob", "sha": resp["sha"]})
        print(f"  blob ok {path} {blob_sha[:8]}")

    # 4) 建 tree（以远端父 tree 为 base）
    st, tree_resp = api("POST", f"/repos/{REPO}/git/trees", {
        "base_tree": remote_parent["tree"]["sha"],
        "tree": entries,
    })
    if st not in (200, 201):
        print(f"[err] 建 tree 失败: {st} {tree_resp}")
        return 1
    print(f"[remote] new tree={tree_resp['sha']}  (local expected {tree_sha})")
    if tree_resp["sha"] != tree_sha:
        print("[warn] tree SHA 与本地不同，commit SHA 将不一致（内容仍等价）")

    # 5) 建 commit（author/committer 逐字复刻）
    def split_ident(line):
        # "Name <email> 1789129837 +0800"
        lt = line.rfind("<")
        gt = line.rfind(">")
        name = line[:lt].strip()
        email = line[lt + 1:gt]
        rest = line[gt + 1:].strip().split()
        return {"name": name, "email": email, "date": f"{rest[0]} {rest[1]}"}

    author = split_ident(author_line)
    # git 的 "date" 需 ISO8601；用 epoch 更稳
    import datetime
    def iso(ts, tz):
        sign = 1 if tz[0] == "+" else -1
        hh, mm = int(tz[1:3]), int(tz[3:5])
        tzinfo = datetime.timezone(sign * datetime.timedelta(hours=hh, minutes=mm))
        return datetime.datetime.fromtimestamp(int(ts), tzinfo).isoformat()

    a_parts = author_line.rsplit(" ", 2)
    a_ident = split_ident(author_line); a_ident["date"] = iso(a_parts[-2], a_parts[-1])
    c_parts = committer_line.rsplit(" ", 2)
    c_ident = split_ident(committer_line); c_ident["date"] = iso(c_parts[-2], c_parts[-1])

    st, commit_resp = api("POST", f"/repos/{REPO}/git/commits", {
        "message": message,
        "tree": tree_resp["sha"],
        "parents": [parent_sha],
        "author": a_ident,
        "committer": c_ident,
    })
    if st not in (200, 201):
        print(f"[err] 建 commit 失败: {st} {commit_resp}")
        return 1
    new_sha = commit_resp["sha"]
    local_sha = git("rev-parse", LOCAL_COMMIT)
    print(f"[remote] new commit={new_sha}  (local {local_sha})")
    if new_sha != local_sha:
        print("[warn] commit SHA 与本地不同——本地将出现分叉，请勿再 push（或 force 对齐）")

    # 6) 更新 refs/heads/main（通过 API，无需 github.com:443）
    st, ref = api("PATCH", f"/repos/{REPO}/git/refs/heads/main", {"sha": new_sha, "force": False})
    if st != 200:
        print(f"[err] 更新 main 失败: {st} {ref}")
        return 1
    print(f"[remote] main -> {ref['object']['sha']}")

    # 7) 建 tag ref（未指定 tag 名则跳过）
    if not TAG:
        print("[skip] 未指定 tag，仅更新分支")
        print("DONE_LOCAL_PUSH")
        return 0
    st, tref = api("POST", f"/repos/{REPO}/git/refs",
                   {"ref": f"refs/tags/{TAG}", "sha": new_sha})
    if st in (200, 201):
        print(f"[remote] tag {TAG} -> {tref['object']['sha']}")
    elif st == 422:
        st2, tref2 = api("PATCH", f"/repos/{REPO}/git/refs/tags/{TAG}",
                         {"sha": new_sha, "force": True})
        print(f"[remote] tag {TAG} 已存在，更新 -> {tref2.get('object', {}).get('sha')} ({st2})")
    else:
        print(f"[err] 建 tag 失败: {st} {tref}")
        return 1

    print("DONE_LOCAL_PUSH")
    return 0


if __name__ == "__main__":
    sys.exit(main())
