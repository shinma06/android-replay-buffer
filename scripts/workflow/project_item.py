#!/usr/bin/env python3
"""Repository-scoped Project item mutations; opaque IDs are resolved, never input."""
import argparse
import json
import re

from agent_loop import GitHub, OWNER, REPO

PROJECT_NUMBER = 5
CONTENT = '''__typename ... on Issue { id number repository { nameWithOwner } }
    ... on PullRequest { id number repository { nameWithOwner } }'''
ITEM = '''id isArchived project { id number owner { ... on User { login } ... on Organization { login } } }
    content { ''' + CONTENT + ''' }
    fieldValueByName(name:$field) { __typename
      ... on ProjectV2ItemFieldSingleSelectValue { name optionId
        field { ... on ProjectV2SingleSelectField { id name } } } }'''


def graphql(gh, query, **variables):
    result = gh.api('graphql', 'POST', {'query': query, 'variables': variables})
    if not isinstance(result, dict) or result.get('errors') or not isinstance(result.get('data'), dict):
        raise ValueError('Project response is incomplete or contains errors; no retry')
    return result['data']


def complete(connection):
    if (not isinstance(connection, dict) or
            not isinstance(connection.get('pageInfo'), dict) or
            connection['pageInfo'].get('hasNextPage') is not False or
            not isinstance(connection.get('nodes'), list) or
            any(not isinstance(node, dict) for node in connection['nodes'])):
        raise ValueError('Partial or unknown Project connection; refuse mutation')
    return connection['nodes']


def identity(content, kind, number, content_id=None):
    if (not isinstance(content, dict) or content.get('__typename') != kind or
            content.get('number') != number or
            not isinstance(content.get('repository'), dict) or
            content['repository'].get('nameWithOwner') != REPO or
            not isinstance(content.get('id'), str) or not content['id'] or
            content_id is not None and content['id'] != content_id):
        raise ValueError('Issue/PR repository, kind, number or ID mismatch')


def project_identity(project, project_id=None):
    if (not isinstance(project, dict) or project.get('number') != PROJECT_NUMBER or
            not isinstance(project.get('owner'), dict) or project['owner'].get('login') != OWNER or
            not isinstance(project.get('id'), str) or not project['id'] or
            project_id is not None and project['id'] != project_id):
        raise ValueError('Project owner, number or ID mismatch')


def item_identity(item, project, content):
    if not isinstance(item, dict) or not isinstance(item.get('id'), str) or not item['id']:
        raise ValueError('Unknown Project item ID')
    if type(item.get('isArchived')) is not bool:
        raise ValueError('Unknown Project item archive state')
    project_identity(item.get('project'), project['id'])
    identity(item.get('content'), content['__typename'], content['number'], content['id'])


def snapshot(gh, kind, number, field):
    selector = 'issue' if kind == 'Issue' else 'pullRequest'
    query = '''query($owner:String!, $repo:String!, $number:Int!, $project:Int!, $field:String!) {
      user(login:$owner) { projectV2(number:$project) { id number owner { ... on User { login } }
        repositories(first:100) { nodes { nameWithOwner } pageInfo { hasNextPage } }
        fields(first:100) { nodes { __typename ... on ProjectV2SingleSelectField {
          id name options { id name } } } pageInfo { hasNextPage } } } }
      repository(owner:$owner,name:$repo) { nameWithOwner content:''' + selector + '''(number:$number) {
        __typename id number repository { nameWithOwner }
        projectItems(first:100,includeArchived:true) { nodes { ''' + ITEM + ''' }
          pageInfo { hasNextPage } } } } }'''
    data = graphql(gh, query, owner=OWNER, repo=REPO.split('/')[1], number=number,
                   project=PROJECT_NUMBER, field=field)
    project = (data.get('user') or {}).get('projectV2')
    project_identity(project)
    repositories = complete(project.get('repositories'))
    if not all(isinstance(r.get('nameWithOwner'), str) for r in repositories) or \
            not any(r['nameWithOwner'] == REPO for r in repositories):
        raise ValueError('Project is not linked to the expected repository')
    fields = complete(project.get('fields'))
    repository = data.get('repository') or {}
    if repository.get('nameWithOwner') != REPO:
        raise ValueError('Repository lookup mismatch')
    content = repository.get('content')
    identity(content, kind, number)
    items = complete(content.get('projectItems'))
    matches = []
    for item in items:
        item_project = item.get('project') or {}
        if (item_project.get('id') == project['id'] or
                item_project.get('number') == PROJECT_NUMBER and
                item_project.get('owner', {}).get('login') == OWNER):
            item_identity(item, project, content)
            matches.append(item)
        elif not item_project.get('id') or not item_project.get('owner', {}).get('login'):
            raise ValueError('Unknown item Project identity')
    if len(matches) > 1:
        raise ValueError('Duplicate Project items; reconcile without mutation')
    return project, content, matches[0] if matches else None, fields


def single_select(fields, name, value):
    matches = [f for f in fields if f.get('name') == name]
    if len(matches) != 1 or matches[0].get('__typename') != 'ProjectV2SingleSelectField':
        raise ValueError('Unknown or ambiguous Project field')
    field = matches[0]
    options = field.get('options')
    if not field.get('id') or not isinstance(options, list) or \
            any(not isinstance(o, dict) or not o.get('id') or not o.get('name') for o in options):
        raise ValueError('Incomplete Project field options')
    selected = [o for o in options if o['name'] == value]
    if len(selected) != 1:
        raise ValueError('Unknown or ambiguous Project option')
    return field['id'], selected[0]['id']


def mutate(gh, action, url, field='Status', value=None):
    """Mutate only a freshly resolved own-repository item and verify the result by readback.

    A post-write error can leave changed remote state; inspect it before retrying through this entry.
    """
    match = re.fullmatch(r'https://github\.com/' + re.escape(REPO) + r'/(issues|pull)/([1-9][0-9]*)', url)
    if not match or action not in ('add', 'set', 'remove'):
        raise ValueError('Only an exact own-repository Issue/PR URL is allowed')
    if field not in ('Status', 'Relationship Status') or (action == 'set') != (value is not None):
        raise ValueError('Set requires a supported field and value; other actions take no value')
    kind = 'Issue' if match[1] == 'issues' else 'PullRequest'
    number = int(match[2])
    project, content, item, fields = snapshot(gh, kind, number, field)
    if action == 'add' and item and item['isArchived']:
        raise ValueError('Item is archived; owner must reconcile restoration before adding')
    if (action == 'add' and item) or (action == 'remove' and item is None):
        return {'action': 'unchanged', 'url': url, 'item': item['id'] if item else None}
    if action == 'set' and item is None:
        raise ValueError('Item is absent; add explicitly before setting fields')
    variables = {'project': project['id'], 'field': field}
    if action == 'add':
        variables['content'] = content['id']
        query = '''mutation($project:ID!, $content:ID!, $field:String!) {
          result:addProjectV2ItemById(input:{projectId:$project,contentId:$content}) {
            item { ''' + ITEM + ''' } } }'''
    elif action == 'set':
        field_id, option_id = single_select(fields, field, value)
        old = item.get('fieldValueByName')
        if old and old.get('optionId') == option_id and old.get('field', {}).get('id') == field_id:
            return {'action': 'unchanged', 'url': url, 'item': item['id']}
        variables.update(item=item['id'], fieldId=field_id, option=option_id)
        query = '''mutation($project:ID!, $item:ID!, $fieldId:ID!, $option:String!, $field:String!) {
          result:updateProjectV2ItemFieldValue(input:{projectId:$project,itemId:$item,
            fieldId:$fieldId,value:{singleSelectOptionId:$option}}) {
            item:projectV2Item { ''' + ITEM + ''' } } }'''
    else:
        variables = {'project': project['id'], 'item': item['id']}
        query = '''mutation($project:ID!, $item:ID!) {
          result:deleteProjectV2Item(input:{projectId:$project,itemId:$item}) { deletedItemId } }'''
    # shortcut: direct credential holders can bypass this entry; native admission control is needed for all routes.
    result = graphql(gh, query, **variables).get('result') or {}
    if action == 'remove':
        if result.get('deletedItemId') != item['id']:
            raise ValueError('Delete response mismatch; inspect state before retry')
    else:
        returned = result.get('item') or {}
        item_identity(returned, project, content)
        if item and returned['id'] != item['id']:
            raise ValueError('Mutation returned another item; inspect state before retry')
    after_project, after_content, after, _ = snapshot(gh, kind, number, field)
    project_identity(after_project, project['id'])
    identity(after_content, kind, number, content['id'])
    if action == 'remove':
        if after is not None:
            raise ValueError('Item removal was not confirmed; no automatic retry')
    elif after is None or after['id'] != returned['id']:
        raise ValueError('Item readback mismatch; no automatic retry')
    elif action == 'add' and after['isArchived']:
        raise ValueError('Added item is archived; owner must reconcile, no automatic retry')
    elif action == 'set':
        current = after.get('fieldValueByName') or {}
        if (current.get('optionId') != option_id or current.get('name') != value or
                current.get('field', {}).get('id') != field_id):
            raise ValueError('Field readback mismatch; no automatic retry')
    return {'action': action, 'url': url, 'item': after['id'] if after else None}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('add', 'set', 'remove'))
    parser.add_argument('url')
    parser.add_argument('--field', choices=('Status', 'Relationship Status'), default='Status')
    parser.add_argument('--value')
    args = parser.parse_args()
    print(json.dumps(mutate(GitHub(), args.action, args.url, args.field, args.value)))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, RuntimeError) as error:
        raise SystemExit(str(error))
