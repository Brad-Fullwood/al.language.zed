page 98101 "Live Contract API"
{
    PageType = API;
    APIPublisher = 'alzed';
    APIGroup = 'contract';
    APIVersion = 'v1.0';
    EntityName = 'liveCustomer';
    EntitySetName = 'liveCustomers';
    SourceTable = Customer;
    ODataKeyFields = SystemId;
    DelayedInsert = true;
    Editable = false;

    layout
    {
        area(Content)
        {
            repeater(Rows)
            {
                field(id; Rec.SystemId)
                {
                }
                field(number; Rec."No.")
                {
                }
                field(displayName; Rec.Name)
                {
                }
            }
        }
    }

    // The snapshot step sets a snappoint on the SNAPPOINT line, and the
    // profiling step samples the loop. Standard API pages run Microsoft code,
    // which a sampling profile does not record.
    trigger OnAfterGetRecord()
    var
        Counter: Integer;
        Total: Integer;
    begin
        for Counter := 1 to 200000 do
            Total += Counter mod 7;
        Total += 1; // LIVE_BC_SNAPPOINT
    end;
}
