"""Read-back verified QA transfer. Called only by the single coordinator after merge."""
import re
from issue_schema import validate_issue
from verification import metadata, validate_change


def handoff(gh, repo, pr, origin, change):
    number, path, mode = metadata(pr)
    if mode != 'develop' or not pr.get('merged') or not re.fullmatch(r'[0-9a-f]{40}', pr.get('merge_commit_sha', '')):
        raise ValueError('QA transfer requires a confirmed develop merge')
    axes = validate_issue(origin)
    validate_change(change, number, 'GUI: required' in pr['body'])
    marker = f'<!-- issue-qa-handoff:v1 origin={number} -->'
    # List all Issues (not search indexing) so a lost create response is retryable.
    candidates = [x for x in gh.pages(f'repos/{repo}/issues?state=all&per_page=100')
                  if 'pull_request' not in x and marker in (x.get('body') or '')]
    if len(candidates) > 1:
        raise ValueError('Multiple QA Issues match origin; reconcile before closing')
    record = f'<!-- issue-qa-record:v1 origin={number} pr={pr["number"]} merge={pr["merge_commit_sha"]} -->'
    source = (record + f'\n元Issue: #{number}\nPR: #{pr["number"]}\nmerge SHA: {pr["merge_commit_sha"]}\n'
              f'Verification: https://github.com/{repo}/blob/{pr["merge_commit_sha"]}/{path}\n')
    if not change['cases']:
        if candidates:
            raise ValueError('Existing QA requires explicit reconciliation before a no-Case transfer')
        # The existing release tracker owns main integration, not an artificial GUI Case.
        release = gh.issue(10)
        if validate_issue(release)['type'] != 'tracking' or release['state'] != 'open':
            raise ValueError('Release tracker #10 must be open before transfer')
        receipt = source + '\nCaseなし。main反映はこのrelease追跡でPMがpromotion PRと履歴を照合する。製品/GUI合格を意味しない。'
        backlink = source + '\n実装はdevelopへ統合。独立した残試験なし。main反映は #10 へ移管（未確認）。'
        for destination, text in ((10, receipt), (number, backlink)):
            if not any(c['body'] == text for c in gh.comments(destination)):
                gh.comment(destination, text)
            if not any(c['body'] == text for c in gh.comments(destination)):
                raise ValueError('Release transfer readback failed')
        return None
    payload = (f'試験内容ドキュメント: https://github.com/{repo}/blob/main/docs/verification/human-qa.md\n' + source +
               '\n## 受入・次操作・依存\n'
               '固定出典の全Case・前提・操作・期待・actor・再確認条件を正本とし、固定候補/buildで残試験を確認する。\n'
               'Case: ' + ', '.join(case['id'] for case in change['cases']) + '\n'
               '担当: PM（実行担当は着手時に割当）。次操作: 候補と未達を照合して試験計画を確定する。'
               '全体の現在進捗は #27、人間/REALの延期範囲は #65、main反映は #10 で追跡する。'
               '元IssueのcloseはGUI pass/main反映を意味しない。既存の観察は履歴であり新候補のpassではない。'
               '製品failは修正Issue/PRへ紐づける。試験が完了しmain反映だけが残る場合は、'
               '#10への双方向移管/readback後にQAを終了できる。初期段階だけの完了は正式Case全体のpassではない。')
    milestone = origin.get('milestone')
    if not candidates:
        summary = re.sub(r'^\[[^]]+\]\s*', '', origin['title'])
        data = {'title': f'[試験] #{number} {summary}', 'body': marker + '\n' + payload,
                'labels': ['type:qa', 'priority:' + axes['priority'], 'status:ready']}
        if milestone:
            data['milestone'] = milestone['number']
        qa = gh.api(f'repos/{repo}/issues', 'POST', data)
    else:
        qa = candidates[0]
    qa = gh.issue(qa['number'])
    if validate_issue(qa)['type'] != 'qa' or marker not in (qa.get('body') or '') or not qa['title'].startswith(f'[試験] #{number} '):
        raise ValueError('QA identity readback failed')
    if qa['state'] != 'open':
        raise ValueError('Existing QA is closed; reconcile remaining verification before closure')
    if milestone and (qa.get('milestone') or {}).get('number') != milestone['number']:
        raise ValueError('QA milestone differs from origin; reconcile before closure')
    children_path = f'repos/{repo}/issues/{number}/sub_issues'
    if not any(x['id'] == qa['id'] for x in gh.pages(children_path + '?per_page=100')):
        gh.api(children_path, 'POST', {'sub_issue_id': qa['id']})
    if not any(x['id'] == qa['id'] for x in gh.pages(children_path + '?per_page=100')):
        raise ValueError('QA parent relationship readback failed')
    if payload not in qa['body']:
        comments = gh.comments(qa['number'])
        if not any(payload == c['body'] for c in comments):
            gh.comment(qa['number'], payload)
        if not any(payload == c['body'] for c in gh.comments(qa['number'])):
            raise ValueError('QA content readback failed')
    link = f'<!-- issue-qa-link:v1 origin={number} qa={qa["number"]} -->'
    text = link + f'\n実装はPR #{pr["number"]} / {pr["merge_commit_sha"]}でdevelopへ統合。残る試験は #{qa["number"]}、main反映は #10。GUI pass/main反映済みではありません。'
    if not any(c['body'] == text for c in gh.comments(number)):
        gh.comment(number, text)
    if not any(c['body'] == text for c in gh.comments(number)):
        raise ValueError('Origin backlink readback failed')
    return qa['number']
