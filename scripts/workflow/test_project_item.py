import copy
import unittest

import project_item as guard

URL = 'https://github.com/shinma06/android-replay-buffer/issues/262'


class FakeGitHub:
    def __init__(self, present=True, kind='Issue'):
        self.project = {'id': 'P5', 'number': 5, 'owner': {'login': 'shinma06'},
                        'repositories': {'nodes': [{'nameWithOwner': guard.REPO}],
                                         'pageInfo': {'hasNextPage': False}},
                        'fields': {'nodes': [{'__typename': 'ProjectV2SingleSelectField', 'id': 'FS',
                                             'name': 'Status', 'options': [{'id': 'OS', 'name': 'In Progress'}]}],
                                   'pageInfo': {'hasNextPage': False}}}
        self.content = {'__typename': kind, 'id': 'C262', 'number': 262,
                        'repository': {'nameWithOwner': guard.REPO}}
        self.item = {'id': 'ITEM262', 'project': {k: self.project[k] for k in ('id', 'number', 'owner')},
                     'content': copy.deepcopy(self.content), 'fieldValueByName': None} if present else None
        self.partial = False
        self.extra = []
        self.mutations = []
        self.errors = None
        self.bad_return = False
        self.bad_readback = False

    def api(self, path, method, payload):
        assert path == 'graphql' and method == 'POST'
        query, variables = payload['query'], payload['variables']
        if query.startswith('query'):
            content = copy.deepcopy(self.content)
            content['projectItems'] = {'nodes': ([copy.deepcopy(self.item)] if self.item else []) + self.extra,
                                       'pageInfo': {'hasNextPage': self.partial}}
            if self.bad_readback and self.mutations:
                content['repository']['nameWithOwner'] = 'elsewhere/foreign'
            return {'errors': self.errors, 'data': {'user': {'projectV2': copy.deepcopy(self.project)},
                    'repository': {'nameWithOwner': guard.REPO, 'content': content}}}
        self.mutations.append(copy.deepcopy(payload))
        if 'addProjectV2ItemById' in query:
            assert variables['content'] == self.content['id']
            self.item = {'id': 'ITEM262', 'project': {k: self.project[k] for k in ('id', 'number', 'owner')},
                         'content': copy.deepcopy(self.content), 'fieldValueByName': None}
        elif 'updateProjectV2ItemFieldValue' in query:
            assert variables['item'] == self.item['id'] and variables['fieldId'] == 'FS'
            self.item['fieldValueByName'] = {'__typename': 'ProjectV2ItemFieldSingleSelectValue',
                                            'name': 'In Progress', 'optionId': 'OS',
                                            'field': {'id': 'FS', 'name': 'Status'}}
        else:
            assert variables['item'] == self.item['id']
            self.item = None
            return {'data': {'result': {'deletedItemId': 'ITEM262'}}}
        item = copy.deepcopy(self.item)
        if self.bad_return:
            item['content']['repository']['nameWithOwner'] = 'elsewhere/foreign'
        return {'data': {'result': {'item': item}}}


class ProjectItemTest(unittest.TestCase):
    def call(self, gh, action, url=URL):
        return guard.mutate(gh, action, url, value='In Progress' if action == 'set' else None)

    def test_own_issue_and_pr_add_set_remove_with_readback(self):
        for kind, route in [('Issue', 'issues'), ('PullRequest', 'pull')]:
            with self.subTest(kind=kind):
                gh = FakeGitHub(False, kind)
                url = URL.replace('issues', route)
                for action in ('add', 'set', 'remove'):
                    self.assertEqual(self.call(gh, action, url)['action'], action)
                self.assertEqual(len(gh.mutations), 3)
                self.assertIsNone(gh.item)

    def test_reuse_and_unchanged_field_do_not_mutate(self):
        gh = FakeGitHub()
        before = copy.deepcopy(gh.item)
        self.assertEqual(self.call(gh, 'add')['action'], 'unchanged')
        self.assertEqual(gh.item, before)
        self.call(gh, 'set')
        self.assertEqual(self.call(gh, 'set')['action'], 'unchanged')
        self.assertEqual(len(gh.mutations), 1)
        gh.item = None
        self.assertEqual(self.call(gh, 'remove')['action'], 'unchanged')
        with self.assertRaises(ValueError):
            self.call(gh, 'set')
        self.assertEqual(len(gh.mutations), 1)

    def test_foreign_and_ambiguous_inputs_cause_zero_mutations(self):
        for action in ('add', 'set', 'remove'):
            for url in (URL.replace(guard.REPO, 'elsewhere/foreign'), URL + '?x=1', URL + '#x',
                        URL.replace('/262', '/0'), URL.replace('https:', 'http:'), 'C262', 'ITEM262'):
                with self.subTest(action=action, url=url):
                    gh = FakeGitHub()
                    with self.assertRaises(ValueError):
                        self.call(gh, action, url)
                    self.assertEqual(gh.mutations, [])

    def test_unknown_foreign_mismatched_and_partial_lookup_never_mutates(self):
        def change(gh, defect):
            if defect == 'foreign-same-number':
                gh.content['repository']['nameWithOwner'] = 'elsewhere/foreign'
            elif defect == 'wrong-content-number':
                gh.content['number'] = 255
            elif defect == 'wrong-kind':
                gh.content['__typename'] = 'PullRequest'
            elif defect == 'wrong-item-content-id':
                gh.item['content']['id'] = 'OTHER'
            elif defect == 'foreign-item-same-number':
                gh.item['content']['repository']['nameWithOwner'] = 'elsewhere/foreign'
            elif defect == 'unknown-content':
                gh.content['id'] = None
            elif defect == 'wrong-project-owner':
                gh.project['owner']['login'] = 'foreign'
            elif defect == 'wrong-project-number':
                gh.project['number'] = 2
            elif defect == 'wrong-project-id':
                gh.item['project']['id'] = 'OTHER'
            elif defect == 'unknown-item-id':
                gh.item['id'] = None
            elif defect == 'no-repository-link':
                gh.project['repositories']['nodes'] = []
            elif defect == 'partial-links':
                gh.project['repositories']['pageInfo']['hasNextPage'] = True
            elif defect == 'partial-fields':
                gh.project['fields']['pageInfo']['hasNextPage'] = True
            elif defect == 'partial-items':
                gh.partial = True
            elif defect == 'unknown-page':
                gh.project['fields']['pageInfo'] = {}
            elif defect == 'duplicate-items':
                gh.extra = [copy.deepcopy(gh.item)]
            elif defect == 'graphql-partial-error':
                gh.errors = [{'message': 'partial'}]
        defects = ('foreign-same-number', 'wrong-content-number', 'wrong-kind', 'wrong-item-content-id',
                   'foreign-item-same-number', 'unknown-content', 'wrong-project-owner',
                   'wrong-project-number', 'wrong-project-id', 'unknown-item-id', 'no-repository-link',
                   'partial-links', 'partial-fields', 'partial-items', 'unknown-page',
                   'duplicate-items', 'graphql-partial-error')
        for action in ('add', 'set', 'remove'):
            for defect in defects:
                with self.subTest(action=action, defect=defect):
                    gh = FakeGitHub()
                    change(gh, defect)
                    with self.assertRaises(ValueError):
                        self.call(gh, action)
                    self.assertEqual(gh.mutations, [])

    def test_unknown_field_or_option_never_mutates(self):
        for defect in ('field', 'option', 'duplicate', 'incomplete'):
            gh = FakeGitHub()
            f = gh.project['fields']['nodes'][0]
            if defect == 'field':
                f['name'] = 'Other'
            elif defect == 'option':
                f['options'][0]['name'] = 'Other'
            elif defect == 'duplicate':
                f['options'].append(copy.deepcopy(f['options'][0]))
            else:
                f['options'][0]['id'] = None
            with self.assertRaises(ValueError):
                self.call(gh, 'set')
            self.assertEqual(gh.mutations, [])

    def test_null_identity_and_connections_never_mutate(self):
        for defect in ('content-repository', 'project-owner', 'page-info', 'project-fields'):
            gh = FakeGitHub()
            if defect == 'content-repository':
                gh.content['repository'] = None
            elif defect == 'project-owner':
                gh.project['owner'] = None
            elif defect == 'page-info':
                gh.project['repositories']['pageInfo'] = None
            else:
                gh.project['fields'] = None
            with self.assertRaises(ValueError):
                self.call(gh, 'set')
            self.assertEqual(gh.mutations, [])

    def test_response_or_readback_mismatch_stops_without_compensating_mutation(self):
        for defect in ('bad_return', 'bad_readback'):
            gh = FakeGitHub(False)
            setattr(gh, defect, True)
            with self.assertRaises(ValueError):
                self.call(gh, 'add')
            self.assertEqual(len(gh.mutations), 1)


if __name__ == '__main__':
    unittest.main()
