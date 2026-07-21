enum 50130 "Work Order Status"
{
    Extensible = true;

    value(0; Pending)
    {
        Caption = 'Pending';
    }
    value(1; Precheck)
    {
        Caption = 'Precheck';
    }
    value(2; Posting)
    {
        Caption = 'Posting';
    }
    value(3; Posted)
    {
        Caption = 'Posted';
    }
    value(4; Failed)
    {
        Caption = 'Failed';
    }
}
