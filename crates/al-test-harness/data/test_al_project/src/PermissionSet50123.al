permissionset 50123 "Test App Full"
{
    Assignable = true;
    Caption = 'Test App - Full Access';

    Permissions =
        tabledata "Test Customer" = RIMD,
        codeunit "Hello World" = X;
}
