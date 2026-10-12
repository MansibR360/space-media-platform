ALTER TABLE user_permissions DROP CONSTRAINT user_permissions_permission_check;
ALTER TABLE user_permissions ADD CONSTRAINT user_permissions_permission_check
    CHECK (permission IN ('manage_library', 'moderate_content', 'admin_all', 'download'));
